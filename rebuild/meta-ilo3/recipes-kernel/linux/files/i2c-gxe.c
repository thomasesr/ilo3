// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE (GXE) I2C/SMBus controller
 *
 * Reverse-engineered from iLO 3 firmware (docs/05 + i2c_mwr/0xe21c8 analysis).
 * The ASIC has 8 DMA-style I2C engines sharing one block. Per engine:
 *   engine regs @ base + eng*0x10 : status +0x00, ctrl +0x01, data +0x04, cmd +0x08
 *   TX/RX buffer @ base + 0x80 + eng*0x10  (+0x200 for engines 6,7), 16 bytes
 *   descriptor  @ base + 0xE0 + eng*4      : status +0, cmd +1, len +2
 * IRQ: one per engine (DT interrupts <8..15> = engines 0..7).
 *
 * CONFIRMED: the three region bases, 16-byte buffer, reset sequence, error bits.
 * BEST-GUESS (flagged GXE_I2C_* / the xfer below): the exact command-byte encoding
 * and the trigger. In the firmware the engine is kicked via an INTEGRITY IODevice
 * call (abstracted), and completion arrives as the per-engine IRQ; here we start
 * by setting descriptor cmd bit0 and poll the descriptor status. VERIFY ON HW.
 */

#include <linux/delay.h>
#include <linux/i2c.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>

#define GXE_I2C_NR_ENG		8
#define GXE_I2C_MAX_LEN		16

/* per-engine register block @ base + eng*0x10 */
#define GXE_ENG_REG(e)		((e) * 0x10)
#define GXE_ENG_STATUS		0x00	/* bit0 busy, bit4 active; err bits below */
#define GXE_ENG_CTRL		0x01	/* write 0 to clear error (0xE0 mask) */
#define GXE_ENG_DATA		0x04	/* reset writes 0x80 then 0x33 here */
#define GXE_ENG_CMD		0x08	/* reset writes 0xB0 then 0x30 here */

/* shared TX/RX buffer @ base + 0x80 + eng*0x10 (+0x200 for eng 6,7) */
#define GXE_BUF(e)		(0x80 + (e) * 0x10 + ((e) > 5 ? 0x200 : 0))

/* per-engine descriptor @ base + 0xE0 + eng*4 */
#define GXE_DESC(e)		(0xE0 + (e) * 4)
#define GXE_DESC_STATUS		0	/* 0x2 = done; bit0 busy, bit2/bit3 error */
#define GXE_DESC_CMD		1	/* set bit0 = start */
#define GXE_DESC_LEN		2	/* byte count */

#define GXE_DESC_DONE		0x02
#define GXE_DESC_ERR		(BIT(2) | BIT(3))

struct gxe_i2c {
	void __iomem	*base;
	struct i2c_adapter adap[GXE_I2C_NR_ENG];
	unsigned int	eng[GXE_I2C_NR_ENG];	/* engine index per adapter */
};

/* abort/reset one engine (CONFIRMED sequence from fw 0xe21c8/0xe30cc) */
static void gxe_i2c_reset(struct gxe_i2c *i2c, unsigned int e)
{
	void __iomem *r = i2c->base + GXE_ENG_REG(e);

	writeb_relaxed(0x00, r + GXE_ENG_CTRL);
	writeb_relaxed(0x80, r + GXE_ENG_DATA);
	writeb_relaxed(0x33, r + GXE_ENG_DATA);
}

/*
 * Run one i2c_msg on one engine. Model (from fw 0xe21c8):
 *   1. copy up to 16 TX bytes into the engine buffer
 *   2. descriptor: len @+2, cmd @+1 (bit0 = start/go)
 *   3. wait for descriptor status @+0 == DONE (0x2); err bits -> reset + fail
 *   4. on read, copy RX bytes back out of the engine buffer
 *
 * CONFIRM-ON-HARDWARE: the command-byte encoding below (addr<<1|rd placed in the
 * cmd nibble) and whether writing cmd bit0 is the trigger vs. a separate kick.
 * The firmware kicks the engine through an INTEGRITY IODevice call we can't call
 * from Linux; the descriptor-write-is-trigger reading is the most likely direct
 * equivalent. If transfers never complete, the trigger is the thing to revisit.
 */
#define GXE_I2C_CMD_START	0x01		/* cmd bit0 = go (CONFIRMED |1) */
#define GXE_I2C_CMD_RD		0x02		/* read direction (BEST GUESS) */

static int gxe_i2c_xfer_msg(struct gxe_i2c *i2c, unsigned int e,
			    struct i2c_msg *msg)
{
	void __iomem *buf  = i2c->base + GXE_BUF(e);
	void __iomem *desc = i2c->base + GXE_DESC(e);
	bool read = !!(msg->flags & I2C_M_RD);
	u8 cmd, status;
	int i, ret;

	if (msg->len > GXE_I2C_MAX_LEN)
		return -EOPNOTSUPP;		/* engine buffer is 16 bytes */

	/* buffer[0] = addressing byte, then payload for writes */
	writeb_relaxed((msg->addr << 1) | (read ? 1 : 0), buf);
	if (!read)
		for (i = 0; i < msg->len; i++)
			writeb_relaxed(msg->buf[i], buf + 1 + i);

	cmd = GXE_I2C_CMD_START | (read ? GXE_I2C_CMD_RD : 0);
	writeb_relaxed(msg->len, desc + GXE_DESC_LEN);
	writeb_relaxed(cmd,      desc + GXE_DESC_CMD);	/* start */

	/* poll status; fw treats 0x2 = done, bit2/bit3 = bus error */
	ret = readb_relaxed_poll_timeout(desc + GXE_DESC_STATUS, status,
					 status & (GXE_DESC_DONE | GXE_DESC_ERR),
					 10, 100 * USEC_PER_MSEC);
	if (ret || (status & GXE_DESC_ERR)) {
		gxe_i2c_reset(i2c, e);
		return ret ? -ETIMEDOUT : -EIO;
	}

	if (read)
		for (i = 0; i < msg->len; i++)
			msg->buf[i] = readb_relaxed(buf + 1 + i);

	return 0;
}

/* one adapter per engine; adap->algo_data carries the engine index */
static int gxe_i2c_master_xfer(struct i2c_adapter *adap,
			       struct i2c_msg *msgs, int num)
{
	struct gxe_i2c *i2c = i2c_get_adapdata(adap);
	unsigned int e = (uintptr_t)adap->algo_data;
	int i, ret;

	for (i = 0; i < num; i++) {
		ret = gxe_i2c_xfer_msg(i2c, e, &msgs[i]);
		if (ret)
			return ret;
	}
	return num;
}

static u32 gxe_i2c_func(struct i2c_adapter *adap)
{
	return I2C_FUNC_I2C | I2C_FUNC_SMBUS_EMUL;
}

static const struct i2c_algorithm gxe_i2c_algo = {
	.master_xfer	= gxe_i2c_master_xfer,
	.functionality	= gxe_i2c_func,
};

static int gxe_i2c_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct gxe_i2c *i2c;
	void __iomem *base;
	int e, ret;

	i2c = devm_kzalloc(dev, sizeof(*i2c), GFP_KERNEL);
	if (!i2c)
		return -ENOMEM;

	base = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(base))
		return PTR_ERR(base);
	i2c->base = base;
	platform_set_drvdata(pdev, i2c);

	/* one i2c_adapter per hardware engine (8 shared-block engines) */
	for (e = 0; e < GXE_I2C_NR_ENG; e++) {
		struct i2c_adapter *a = &i2c->adap[e];

		gxe_i2c_reset(i2c, e);		/* known-good start state */

		a->owner	= THIS_MODULE;
		a->algo		= &gxe_i2c_algo;
		a->algo_data	= (void *)(uintptr_t)e;
		a->dev.parent	= dev;
		a->dev.of_node	= dev->of_node;	/* share the single DT node */
		a->nr		= -1;
		snprintf(a->name, sizeof(a->name), "GXE I2C engine %d", e);
		i2c_set_adapdata(a, i2c);

		ret = i2c_add_adapter(a);
		if (ret) {
			while (--e >= 0)
				i2c_del_adapter(&i2c->adap[e]);
			return ret;
		}
	}

	dev_info(dev, "GXE I2C: %d engines @ %pR\n", GXE_I2C_NR_ENG,
		 platform_get_resource(pdev, IORESOURCE_MEM, 0));
	return 0;
}

static int gxe_i2c_remove(struct platform_device *pdev)
{
	struct gxe_i2c *i2c = platform_get_drvdata(pdev);
	int e;

	for (e = 0; e < GXE_I2C_NR_ENG; e++)
		i2c_del_adapter(&i2c->adap[e]);
	return 0;
}

static const struct of_device_id gxe_i2c_of_match[] = {
	{ .compatible = "hpe,gxe-i2c" },
	{ }
};
MODULE_DEVICE_TABLE(of, gxe_i2c_of_match);

static struct platform_driver gxe_i2c_driver = {
	.probe	= gxe_i2c_probe,
	.remove	= gxe_i2c_remove,
	.driver	= {
		.name		= "gxe-i2c",
		.of_match_table	= gxe_i2c_of_match,
	},
};
module_platform_driver(gxe_i2c_driver);

MODULE_DESCRIPTION("HP iLO 3 Gromit-XE I2C/SMBus driver");
MODULE_LICENSE("GPL");
