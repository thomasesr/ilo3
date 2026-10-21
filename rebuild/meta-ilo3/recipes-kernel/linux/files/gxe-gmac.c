// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE (GXE) GXELAN Ethernet MAC
 *
 * Reverse-engineered from the iLO 3 kernel `umac.c` (docs/11). Two MACs:
 *   MAC0 @ 0xC0004000 = dedicated iLO management port (its own PHY on MDIO)
 *   MAC1 @ 0xC0005000 = sideband/shared NIC (NC-SI to the host LOM, no PHY)
 * Shared MDIO at the MAC0 block (0xC0004080/84). Global reset in the ASIC core
 * block at 0xC00000C0 (MAC0 = bit0x04, MAC1 = bit0x80). IRQ 16 (MAC0)/17 (MAC1).
 *
 * CONFIRMED by disassembly and implemented here: the MDIO bus protocol, the MAC
 * reset, the station-address registers (+0x18/+0x1C/+0x20), and the multicast
 * hash filter (+0x24/+0x28, CRC-32 poly 0x04C11DB6).
 *
 * NOT RECOVERABLE from static RE (see docs/11): the TX/RX DMA ring base/head/tail
 * registers and the on-wire descriptor format — the firmware manages the rings as
 * INTEGRITY DMA descriptor objects, an abstraction above the hardware registers.
 * The datapath (ndo_start_xmit / NAPI poll) below is therefore a clearly-marked
 * scaffold; the descriptor format must be pinned during hardware bring-up before
 * this driver can pass traffic. MDIO + link + MAC config are usable as-is.
 */

#include <linux/crc32.h>
#include <linux/etherdevice.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/of_mdio.h>
#include <linux/of_net.h>
#include <linux/phy.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>

/* MAC register block (offset from base) — CONFIRMED (docs/11 RE pass 2) */
#define GXE_MAC_CTRL		0x14	/* 16-bit control/command (enable/kick) */
#define GXE_MAC_ADDR0		0x18	/* station addr [0:1], big-endian */
#define GXE_MAC_ADDR1		0x1C	/* station addr [2:3] */
#define GXE_MAC_ADDR2		0x20	/* station addr [4:5] */
#define GXE_MAC_HASH0		0x24	/* multicast hash word 0 */
#define GXE_MAC_HASH1		0x28	/* multicast hash word 1 */

/* MDIO — physically in the MAC0 block at 0xC0004080/84 — CONFIRMED */
#define GXE_MDIO_CMD		0x80
#define GXE_MDIO_DATA		0x84
#define GXE_MDIO_READ		BIT(8)	/* set = read, clear = write */
#define GXE_MDIO_GO		BIT(9)	/* trigger; polls to 0 = done */

/* Global reset in the ASIC core block 0xC00000C0 (16-bit) — CONFIRMED */
#define GXE_RST_REG		0xC0		/* offset in the core syscon */
#define GXE_RST_MAC0		BIT(2)		/* 0x04 */
#define GXE_RST_MAC1		BIT(7)		/* 0x80 */

#define GXE_MDIO_TIMEOUT_US	100000

struct gxe_gmac {
	struct net_device	*ndev;
	struct device		*dev;
	void __iomem		*base;		/* MAC register block */
	void __iomem		*mdio;		/* MAC0 block for MDIO (0xC0004000) */
	struct regmap		*core;		/* ASIC core syscon for reset */
	struct mii_bus		*mii;
	int			irq;
	bool			is_mac0;	/* dedicated (MDIO) vs sideband (NC-SI) */
	u32			rst_bit;
};

/* ---------------- MDIO (CONFIRMED) ---------------- */

static int gxe_mdio_read(struct mii_bus *bus, int phy, int reg)
{
	struct gxe_gmac *g = bus->priv;
	u32 cmd = (reg & 0x1f) | ((phy & 0x1f) << 16) | GXE_MDIO_READ;
	u32 v;
	int ret;

	writel(cmd, g->mdio + GXE_MDIO_CMD);
	writel(cmd | GXE_MDIO_GO, g->mdio + GXE_MDIO_CMD);

	ret = readl_poll_timeout(g->mdio + GXE_MDIO_CMD, v, !(v & GXE_MDIO_GO),
				 1, GXE_MDIO_TIMEOUT_US);
	if (ret)
		return ret;

	return readl(g->mdio + GXE_MDIO_DATA) & 0xffff;
}

static int gxe_mdio_write(struct mii_bus *bus, int phy, int reg, u16 val)
{
	struct gxe_gmac *g = bus->priv;
	u32 cmd = (reg & 0x1f) | ((phy & 0x1f) << 16);	/* no READ bit = write */
	u32 v;

	writel(cmd, g->mdio + GXE_MDIO_CMD);
	writel(val, g->mdio + GXE_MDIO_DATA);
	writel(cmd | GXE_MDIO_GO, g->mdio + GXE_MDIO_CMD);

	return readl_poll_timeout(g->mdio + GXE_MDIO_CMD, v, !(v & GXE_MDIO_GO),
				  1, GXE_MDIO_TIMEOUT_US);
}

/* ---------------- MAC config (CONFIRMED) ---------------- */

static void gxe_mac_reset(struct gxe_gmac *g)
{
	if (!g->core)
		return;
	/* pulse the reset bit: set, delay, clear (fw 0x2d44c) */
	regmap_update_bits(g->core, GXE_RST_REG, g->rst_bit, g->rst_bit);
	usleep_range(10, 50);
	regmap_update_bits(g->core, GXE_RST_REG, g->rst_bit, 0);
}

static void gxe_mac_set_hwaddr(struct gxe_gmac *g, const u8 *a)
{
	/* 6 bytes packed big-endian into three 16-bit regs (fw 0x2d59c) */
	writew((a[0] << 8) | a[1], g->base + GXE_MAC_ADDR0);
	writew((a[2] << 8) | a[3], g->base + GXE_MAC_ADDR1);
	writew((a[4] << 8) | a[5], g->base + GXE_MAC_ADDR2);
}

static int gxe_set_mac_address(struct net_device *ndev, void *p)
{
	struct gxe_gmac *g = netdev_priv(ndev);
	int ret = eth_mac_addr(ndev, p);

	if (ret)
		return ret;
	gxe_mac_set_hwaddr(g, ndev->dev_addr);
	return 0;
}

/* Multicast hash: CRC-32 (poly 0x04C11DB6) -> 6-bit index -> 64-bit hash
 * at +0x24/+0x28 (fw 0x2d4c4/0x2d540/0x2d578). */
static void gxe_set_rx_mode(struct net_device *ndev)
{
	struct gxe_gmac *g = netdev_priv(ndev);
	u32 hash[2] = { 0, 0 };
	struct netdev_hw_addr *ha;

	if (ndev->flags & (IFF_PROMISC | IFF_ALLMULTI)) {
		hash[0] = hash[1] = 0xffffffff;
	} else {
		netdev_for_each_mc_addr(ha, ndev) {
			u32 crc = ether_crc(ETH_ALEN, ha->addr);	/* 0x04C11DB6 */
			u32 bit = crc >> 26;				/* top 6 bits */

			hash[bit >> 5] |= BIT(bit & 0x1f);
		}
	}
	writel(hash[0], g->base + GXE_MAC_HASH0);
	writel(hash[1], g->base + GXE_MAC_HASH1);
}

static void gxe_adjust_link(struct net_device *ndev)
{
	struct phy_device *phy = ndev->phydev;

	/* Link/speed/duplex are reported by the fw as "Link: UP %d Mbps %s-Duplex".
	 * The exact +0x14 control-register speed/duplex fields are a best-reading;
	 * program them here once confirmed on hardware. */
	phy_print_status(phy);
}

/* ---------------- datapath (SCAFFOLD — ring format unrecovered) ---------------- */

static netdev_tx_t gxe_start_xmit(struct sk_buff *skb, struct net_device *ndev)
{
	/*
	 * TODO(hardware bring-up): the TX DMA ring base/head/tail registers and the
	 * descriptor format are not recovered from static RE (fw uses INTEGRITY DMA
	 * descriptor objects — docs/11). Until pinned on hardware, drop and count.
	 */
	net_warn_ratelimited("%s: TX datapath not implemented (ring format TBD)\n",
			     ndev->name);
	ndev->stats.tx_dropped++;
	dev_kfree_skb_any(skb);
	return NETDEV_TX_OK;
}

static int gxe_open(struct net_device *ndev)
{
	struct gxe_gmac *g = netdev_priv(ndev);

	gxe_mac_reset(g);
	gxe_mac_set_hwaddr(g, ndev->dev_addr);

	if (g->is_mac0 && ndev->phydev)
		phy_start(ndev->phydev);

	/* TODO(hardware bring-up): allocate + program the TX/RX rings, unmask IRQ,
	 * set the enable bits in CTRL (+0x14) once the register fields are confirmed. */
	netif_start_queue(ndev);
	return 0;
}

static int gxe_stop(struct net_device *ndev)
{
	netif_stop_queue(ndev);
	if (ndev->phydev)
		phy_stop(ndev->phydev);
	return 0;
}

static const struct net_device_ops gxe_netdev_ops = {
	.ndo_open		= gxe_open,
	.ndo_stop		= gxe_stop,
	.ndo_start_xmit		= gxe_start_xmit,
	.ndo_set_mac_address	= gxe_set_mac_address,
	.ndo_set_rx_mode	= gxe_set_rx_mode,
	.ndo_validate_addr	= eth_validate_addr,
};

/* ---------------- probe ---------------- */

static int gxe_mdio_register(struct gxe_gmac *g)
{
	struct device_node *np;
	struct mii_bus *bus;
	int ret;

	bus = devm_mdiobus_alloc(g->dev);
	if (!bus)
		return -ENOMEM;

	bus->name = "gxe-mdio";
	bus->priv = g;
	bus->parent = g->dev;
	bus->read = gxe_mdio_read;
	bus->write = gxe_mdio_write;
	snprintf(bus->id, MII_BUS_ID_SIZE, "%s", dev_name(g->dev));

	/* PHYs live under the "mdio" subnode of the ethernet node */
	np = of_get_child_by_name(g->dev->of_node, "mdio");
	ret = devm_of_mdiobus_register(g->dev, bus, np ? np : g->dev->of_node);
	of_node_put(np);
	if (ret)
		return ret;
	g->mii = bus;
	return 0;
}

static int gxe_gmac_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct net_device *ndev;
	struct gxe_gmac *g;
	struct resource *res;
	int ret;

	ndev = devm_alloc_etherdev(dev, sizeof(*g));
	if (!ndev)
		return -ENOMEM;
	SET_NETDEV_DEV(ndev, dev);

	g = netdev_priv(ndev);
	g->ndev = ndev;
	g->dev = dev;

	g->base = devm_platform_get_and_ioremap_resource(pdev, 0, &res);
	if (IS_ERR(g->base))
		return PTR_ERR(g->base);
	g->mdio = g->base;			/* MDIO lives in this block (MAC0) */
	g->is_mac0 = (res->start == 0xC0004000);
	g->rst_bit = g->is_mac0 ? GXE_RST_MAC0 : GXE_RST_MAC1;

	g->core = syscon_regmap_lookup_by_phandle(dev->of_node, "hpe,core-syscon");
	if (IS_ERR(g->core)) {
		dev_warn(dev, "no hpe,core-syscon; MAC reset disabled\n");
		g->core = NULL;
	}

	g->irq = platform_get_irq(pdev, 0);
	if (g->irq < 0)
		return g->irq;

	ret = of_get_ethdev_address(dev->of_node, ndev);
	if (ret)
		eth_hw_addr_random(ndev);	/* real addr comes from NVRAM/netcfg */

	ndev->netdev_ops = &gxe_netdev_ops;

	gxe_mac_reset(g);

	/* MAC0 drives a PHY over MDIO; MAC1 is NC-SI (no PHY bus) */
	if (g->is_mac0) {
		ret = gxe_mdio_register(g);
		if (ret)
			return dev_err_probe(dev, ret, "MDIO register failed\n");

		ret = of_phy_register_fixed_link(dev->of_node);
		if (!ret || ret == -ENODEV) {
			struct phy_device *phy;

			phy = of_phy_get_and_connect(ndev, dev->of_node,
						     gxe_adjust_link);
			if (phy)
				phy_attached_info(phy);
		}
	}

	platform_set_drvdata(pdev, ndev);

	ret = devm_register_netdev(dev, ndev);
	if (ret)
		return dev_err_probe(dev, ret, "register_netdev failed\n");

	dev_info(dev, "GXELAN %s MAC @ %pR, IRQ %d\n",
		 g->is_mac0 ? "dedicated(MDIO)" : "sideband(NC-SI)", res, g->irq);
	return 0;
}

static const struct of_device_id gxe_gmac_of_match[] = {
	{ .compatible = "hpe,gxe-gmac" },
	{ }
};
MODULE_DEVICE_TABLE(of, gxe_gmac_of_match);

static struct platform_driver gxe_gmac_driver = {
	.probe	= gxe_gmac_probe,
	.driver	= {
		.name		= "gxe-gmac",
		.of_match_table	= gxe_gmac_of_match,
	},
};
module_platform_driver(gxe_gmac_driver);

MODULE_DESCRIPTION("HP iLO 3 Gromit-XE GXELAN Ethernet MAC driver");
MODULE_LICENSE("GPL");
