// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE (GXE) PECI controller
 *
 * Reverse-engineered from iLO 3 firmware (`src\peci.c`: peci_init `0x11c334`,
 * run_peci_cmd `0x11c7a4`, low-level xfer `0x11cb68`; see docs/26). The native
 * PECI engine (CPU DTS temperature, RdPkgConfig, RdPCIConfig) lives in the
 * 0x01F00000 peripheral window at base 0x01F00400 — NOT the CPLD +0x68, which is
 * only the multi-node (XNC) target MUX. Completion is IRQ 60 (docs/12).
 *
 * Register block (base 0x01F00400, CONFIRMED from fw disassembly):
 *   +0x00 CMD    u32  write a command-descriptor (buffer pointer) = start xfer
 *   +0x04 STATUS u32  (val & ~1) != 0 => bus error; IODevice wait returns 0xD1 on timeout
 *   +0x08 TIMING u32  PECI_BIT_TIMING_CONFIGURATION
 *   +0x0C ENABLE u32  write 0x80 to enable the engine
 * Bit-timing is selected by the rev strap 0x01F100A0 bits[23:16]:
 *   <= 0x04 -> 0x3D090010 (slow), else 0x61A80019 (fast).
 * Multi-socket target MUX: CPLD 0x01F14068 (bits[5:0] target; XNC boxes only) — a
 * single-socket G7 leaves it at default.
 *
 * CONFIRM-ON-HARDWARE (flagged): the exact command-descriptor byte layout written
 * to CMD and the response-buffer mechanism (the fw hands the engine a RAM buffer
 * pointer and the INTEGRITY IODPECI IODevice/DMA moves bytes + raises IRQ 60; the
 * descriptor fields {flag=1, cmd, params…} are only partially recovered). The init
 * path, the four engine registers, the timing values, the 0xD1 timeout and the
 * STATUS error test ARE confirmed.
 */

#include <linux/delay.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/peci.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>
#include <linux/mfd/syscon.h>

#define GXE_PECI_CMD		0x00
#define GXE_PECI_STATUS		0x04
#define GXE_PECI_TIMING		0x08
#define GXE_PECI_ENABLE		0x0C

#define GXE_PECI_ENABLE_ON	0x80
#define GXE_PECI_STATUS_ERR	0xFFFFFFFEu	/* any bit above bit0 set = error */
#define GXE_PECI_TIMEOUT_CODE	0xD1		/* IODevice wait timeout */

/* bit-timing presets, selected by the rev strap (fw peci_init) */
#define GXE_PECI_TIMING_SLOW	0x3D090010u
#define GXE_PECI_TIMING_FAST	0x61A80019u
#define GXE_PECI_STRAP		0x01F100A0u	/* rev strap, bits[23:16] */
#define GXE_PECI_STRAP_SLOW_MAX	0x04

/* CPLD multi-node target MUX (XNC only) */
#define GXE_PECI_MUX_TARGET	0x68		/* in the CPLD regmap, bits[5:0] */

struct gxe_peci {
	struct device	*dev;
	void __iomem	*base;		/* 0x01F00400 engine block */
	struct regmap	*cpld;		/* 0x01F14000 CPLD, for the target MUX (optional) */
	struct completion done;
	int		irq;
};

static void gxe_peci_hw_init(struct gxe_peci *p)
{
	void __iomem *strap;
	u32 timing = GXE_PECI_TIMING_FAST;

	/* pick bit timing from the rev strap (fw: 0x01F100A0 bits[23:16] <= 4 -> slow) */
	strap = ioremap(GXE_PECI_STRAP, 4);
	if (strap) {
		u32 rev = (readl(strap) >> 16) & 0xff;

		if (rev <= GXE_PECI_STRAP_SLOW_MAX)
			timing = GXE_PECI_TIMING_SLOW;
		iounmap(strap);
	}

	writel(timing, p->base + GXE_PECI_TIMING);
	writel(GXE_PECI_ENABLE_ON, p->base + GXE_PECI_ENABLE);
	dev_dbg(p->dev, "PECI bit timing = %#x\n", timing);
}

static irqreturn_t gxe_peci_isr(int irq, void *data)
{
	struct gxe_peci *p = data;

	complete(&p->done);
	return IRQ_HANDLED;
}

/*
 * One PECI transaction. The Linux PECI core hands us the raw command bytes in
 * req->tx (first byte is the client address nibble handling done by the core via
 * @addr) and wants rx_len bytes back in req->rx.
 *
 * GXE model (from fw low-level xfer 0x11cb68): assemble a command descriptor in a
 * DMA-coherent buffer, write its bus address to CMD (= start), wait for IRQ 60,
 * then read STATUS: timeout => 0xD1, any bit above bit0 => bus error. On success
 * the engine has written the response into the buffer's response area.
 *
 * CONFIRM-ON-HARDWARE: the descriptor field order below and the response offset
 * are the best-effort reconstruction (fw builds {flag=1, cmd, params…}); verify
 * against the IODPECI engine before trusting readings.
 */
#define GXE_PECI_DESC_SZ	0x40

static int gxe_peci_xfer(struct peci_controller *controller, u8 addr,
			 struct peci_request *req)
{
	struct gxe_peci *p = dev_get_drvdata(controller->dev.parent);
	dma_addr_t dma;
	u8 *buf;
	u32 status;
	int ret, i, n = 0;

	if (req->tx_len + 8 > GXE_PECI_DESC_SZ)
		return -EINVAL;

	buf = dma_alloc_coherent(p->dev, GXE_PECI_DESC_SZ, &dma, GFP_KERNEL);
	if (!buf)
		return -ENOMEM;

	/* descriptor (BEST-EFFORT layout): flag, client addr, tx/rx lengths, payload */
	buf[n++] = 0x01;			/* "valid"/segment flag (fw const 1) */
	buf[n++] = addr;			/* PECI client address (0x30 + socket) */
	buf[n++] = req->tx_len;
	buf[n++] = req->rx_len;
	for (i = 0; i < req->tx_len; i++)
		buf[n++] = req->tx[i];

	reinit_completion(&p->done);
	writel((u32)dma, p->base + GXE_PECI_CMD);	/* start */

	ret = wait_for_completion_timeout(&p->done, msecs_to_jiffies(50));
	if (!ret) {
		ret = -ETIMEDOUT;
		goto out;
	}

	status = readl(p->base + GXE_PECI_STATUS);
	if (status == GXE_PECI_TIMEOUT_CODE || (status & GXE_PECI_STATUS_ERR)) {
		ret = -EIO;
		goto out;
	}

	/* response area follows the command in the buffer (offset = CONFIRM-ON-HW) */
	for (i = 0; i < req->rx_len; i++)
		req->rx[i] = buf[n + i];
	ret = 0;

out:
	dma_free_coherent(p->dev, GXE_PECI_DESC_SZ, buf, dma);
	return ret;
}

static const struct peci_controller_ops gxe_peci_ops = {
	.xfer = gxe_peci_xfer,
};

static int gxe_peci_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct peci_controller *controller;
	struct gxe_peci *p;
	int ret;

	p = devm_kzalloc(dev, sizeof(*p), GFP_KERNEL);
	if (!p)
		return -ENOMEM;

	p->dev = dev;
	init_completion(&p->done);

	p->base = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(p->base))
		return PTR_ERR(p->base);

	/* optional handle to the CPLD for the multi-node target MUX (XNC boxes) */
	p->cpld = syscon_regmap_lookup_by_phandle(dev->of_node, "hpe,cpld");
	if (IS_ERR(p->cpld))
		p->cpld = NULL;

	p->irq = platform_get_irq(pdev, 0);
	if (p->irq < 0)
		return p->irq;
	ret = devm_request_irq(dev, p->irq, gxe_peci_isr, 0, "gxe-peci", p);
	if (ret)
		return ret;

	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(32));
	if (ret)
		return ret;

	gxe_peci_hw_init(p);

	controller = devm_peci_controller_add(dev, &gxe_peci_ops);
	if (IS_ERR(controller))
		return PTR_ERR(controller);

	platform_set_drvdata(pdev, p);
	dev_info(dev, "GXE PECI controller @ IRQ %d\n", p->irq);
	return 0;
}

static const struct of_device_id gxe_peci_of_match[] = {
	{ .compatible = "hpe,gxe-peci" },
	{ }
};
MODULE_DEVICE_TABLE(of, gxe_peci_of_match);

static struct platform_driver gxe_peci_driver = {
	.probe	= gxe_peci_probe,
	.driver	= {
		.name		= "gxe-peci",
		.of_match_table	= gxe_peci_of_match,
	},
};
module_platform_driver(gxe_peci_driver);

MODULE_DESCRIPTION("HP iLO 3 Gromit-XE PECI controller");
MODULE_LICENSE("GPL");
