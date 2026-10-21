// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE (GXE) fan controller — PWM/tach CPLD scan-chain hwmon
 *
 * Reverse-engineered from iLO 3 firmware (fan dispatcher 0x123b50, RESYNC
 * 0x123ae4, tach reader 0x122bac; see docs/25,27). The Gromit-XE does NOT expose
 * a flat per-fan PWM-duty / tach-count register array. PWM (open-drain FET bits)
 * and tach ride a serial shift register ("scan chain") latched through the CPLD,
 * so this is a net-new hwmon driver (no aspeed-pwm-tacho analogue). It presents
 * standard pwmN / fanN_input to phosphor-pid-control.
 *
 * Fan block base 0x01F00200 (CONFIRMED offsets):
 *   +0x0C CTRL      low 3 bits = mode (2 = update)
 *   +0x10..+0x17    8-byte PWM/FET scan-chain DATA
 *   +0x18 TACH_CTRL write 0xFA = arm a tach sample
 *   +0x1C TACH_CFG  tach config word
 * Scan-chain latch strobe: CPLD 0x01F1400B bit1 (set, delay, clear) — reached via
 * an "hpe,cpld" syscon phandle.
 *
 * CONFIRM-ON-HARDWARE (flagged below): (1) whether a channel's duty is a value
 * latched in a DATA byte or a software-timed FET toggle; this driver treats each
 * DATA byte as a 0-255 duty. (2) per-fan chain position (SDR/board-defined — from
 * DT). (3) the tach result register + raw->RPM scale (the fw reads tach through
 * the SDR descriptor framework, not a single pinned MMIO).
 */

#include <linux/bitops.h>
#include <linux/delay.h>
#include <linux/hwmon.h>
#include <linux/io.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/regmap.h>

#define GXE_FAN_CTRL		0x0C
#define GXE_FAN_CTRL_MODE_MASK	0x07
#define GXE_FAN_CTRL_MODE_UPDATE 0x02
#define GXE_FAN_DATA(n)		(0x10 + (n))	/* 8 PWM/FET bytes, n = 0..7 */
#define GXE_FAN_DATA_LAST	0x17
#define GXE_FAN_TACH_CTRL	0x18
#define GXE_FAN_TACH_CFG	0x1C
#define GXE_FAN_TACH_ARM	0xFA		/* write to TACH_CTRL to sample */
#define GXE_FAN_NR_CHAN		8		/* 8 scan-chain DATA bytes */

/* CPLD scan-chain latch strobe (0x01F14000 block) */
#define GXE_CPLD_FAN_SYNC	0x0B
#define GXE_CPLD_SYNC_BIT	BIT(1)

/* TACH result + conversion — CONFIRM-ON-HARDWARE (see file header note 3) */
#define GXE_FAN_TACH_DATA(n)	(0x10 + (n))	/* raw 16b appears per-fan; best-guess */
#define GXE_FAN_TACH_SETTLE_US	1000

struct gxe_fan {
	struct device	*dev;
	void __iomem	*base;		/* 0x01F00200 fan block */
	struct regmap	*cpld;		/* for the latch strobe (optional) */
	u32		nfans;		/* from DT, <= GXE_FAN_NR_CHAN */
	u32		tach_div;	/* raw->RPM divisor from DT (flagged) */
};

/* Pulse the CPLD scan-chain latch strobe to shift the DATA bits out (RESYNC). */
static void gxe_fan_latch(struct gxe_fan *f)
{
	if (!f->cpld)
		return;
	regmap_update_bits(f->cpld, GXE_CPLD_FAN_SYNC,
			   GXE_CPLD_SYNC_BIT, GXE_CPLD_SYNC_BIT);
	udelay(100);
	regmap_update_bits(f->cpld, GXE_CPLD_FAN_SYNC, GXE_CPLD_SYNC_BIT, 0);
}

/* Push one channel's duty into the scan chain and latch it. */
static void gxe_fan_set_pwm(struct gxe_fan *f, unsigned int ch, u8 duty)
{
	u8 ctrl;

	writeb(duty, f->base + GXE_FAN_DATA(ch));

	ctrl = readb(f->base + GXE_FAN_CTRL);
	ctrl = (ctrl & ~GXE_FAN_CTRL_MODE_MASK) | GXE_FAN_CTRL_MODE_UPDATE;
	writeb(ctrl, f->base + GXE_FAN_CTRL);

	gxe_fan_latch(f);
}

/* Arm a tach sample and read the raw count for one fan. CONFIRM-ON-HARDWARE:
 * the result register + raw->RPM scale (fw reads via the SDR descriptor path). */
static u16 gxe_fan_read_tach(struct gxe_fan *f, unsigned int ch)
{
	writeb(GXE_FAN_TACH_ARM, f->base + GXE_FAN_TACH_CTRL);
	udelay(GXE_FAN_TACH_SETTLE_US);
	return readw(f->base + GXE_FAN_TACH_DATA(ch));
}

/* ---------------- hwmon ---------------- */

static umode_t gxe_fan_is_visible(const void *data, enum hwmon_sensor_types type,
				  u32 attr, int ch)
{
	const struct gxe_fan *f = data;

	if (ch >= f->nfans)
		return 0;
	switch (type) {
	case hwmon_pwm:
		return 0644;			/* read/write duty */
	case hwmon_fan:
		return 0444;			/* read RPM */
	default:
		return 0;
	}
}

static int gxe_fan_read(struct device *dev, enum hwmon_sensor_types type,
			u32 attr, int ch, long *val)
{
	struct gxe_fan *f = dev_get_drvdata(dev);
	u16 raw;

	switch (type) {
	case hwmon_pwm:
		*val = readb(f->base + GXE_FAN_DATA(ch));
		return 0;
	case hwmon_fan:
		raw = gxe_fan_read_tach(f, ch);
		/* RPM = raw / tach_div (board constant; 0 => report raw) */
		*val = (f->tach_div && raw) ? (raw / f->tach_div) : raw;
		return 0;
	default:
		return -EOPNOTSUPP;
	}
}

static int gxe_fan_write(struct device *dev, enum hwmon_sensor_types type,
			 u32 attr, int ch, long val)
{
	struct gxe_fan *f = dev_get_drvdata(dev);

	if (type != hwmon_pwm)
		return -EOPNOTSUPP;
	if (val < 0 || val > 255)
		return -EINVAL;

	gxe_fan_set_pwm(f, ch, val);
	return 0;
}

static const struct hwmon_ops gxe_fan_hwmon_ops = {
	.is_visible	= gxe_fan_is_visible,
	.read		= gxe_fan_read,
	.write		= gxe_fan_write,
};

/* 8 PWM + 8 fan channels; is_visible masks to f->nfans at runtime. */
static const struct hwmon_channel_info * const gxe_fan_info[] = {
	HWMON_CHANNEL_INFO(pwm,
		HWMON_PWM_INPUT, HWMON_PWM_INPUT, HWMON_PWM_INPUT, HWMON_PWM_INPUT,
		HWMON_PWM_INPUT, HWMON_PWM_INPUT, HWMON_PWM_INPUT, HWMON_PWM_INPUT),
	HWMON_CHANNEL_INFO(fan,
		HWMON_F_INPUT, HWMON_F_INPUT, HWMON_F_INPUT, HWMON_F_INPUT,
		HWMON_F_INPUT, HWMON_F_INPUT, HWMON_F_INPUT, HWMON_F_INPUT),
	NULL,
};

static const struct hwmon_chip_info gxe_fan_chip_info = {
	.ops	= &gxe_fan_hwmon_ops,
	.info	= gxe_fan_info,
};

static int gxe_fan_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct device *hwmon;
	struct gxe_fan *f;

	f = devm_kzalloc(dev, sizeof(*f), GFP_KERNEL);
	if (!f)
		return -ENOMEM;

	f->dev = dev;
	f->base = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(f->base))
		return PTR_ERR(f->base);

	f->cpld = syscon_regmap_lookup_by_phandle(dev->of_node, "hpe,cpld");
	if (IS_ERR(f->cpld)) {
		dev_warn(dev, "no hpe,cpld phandle; scan-chain latch disabled\n");
		f->cpld = NULL;
	}

	if (of_property_read_u32(dev->of_node, "hpe,num-fans", &f->nfans))
		f->nfans = 6;			/* FAN_1..6_TACH default */
	if (f->nfans > GXE_FAN_NR_CHAN)
		f->nfans = GXE_FAN_NR_CHAN;
	of_property_read_u32(dev->of_node, "hpe,tach-divisor", &f->tach_div);

	platform_set_drvdata(pdev, f);

	hwmon = devm_hwmon_device_register_with_info(dev, "gxe_fan", f,
						     &gxe_fan_chip_info, NULL);
	if (IS_ERR(hwmon))
		return PTR_ERR(hwmon);

	dev_info(dev, "GXE fan scan-chain: %u fans\n", f->nfans);
	return 0;
}

static const struct of_device_id gxe_fan_of_match[] = {
	{ .compatible = "hpe,gxe-fan-scanchain" },
	{ }
};
MODULE_DEVICE_TABLE(of, gxe_fan_of_match);

static struct platform_driver gxe_fan_driver = {
	.probe	= gxe_fan_probe,
	.driver	= {
		.name		= "gxe-fan",
		.of_match_table	= gxe_fan_of_match,
	},
};
module_platform_driver(gxe_fan_driver);

MODULE_DESCRIPTION("HP iLO 3 Gromit-XE fan scan-chain hwmon driver");
MODULE_LICENSE("GPL");
