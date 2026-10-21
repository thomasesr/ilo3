// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE system CPLD / power-microcontroller GPIO
 *
 * The 0x01F14000 block (docs/08,09) is an 8-bit register window fronting the
 * system CPLD + power micro. It carries scattered discrete signals — chassis
 * UID LED, CPLD status/version, host-power sequencing, PSU enable/presence,
 * PECI, pwrmgr. This driver exposes the window as a flat bit-addressable
 * gpio_chip so OpenBMC can wire the discrete lines (gpio-leds for UID,
 * phosphor-gpio-monitor for the button/PSU presence, x86-power-control).
 *
 * Addressing: gpio N = byte-offset (N >> 3) within the window, bit (N & 7).
 * So the chassis UID LED (reg +0x00 bit6, CONFIRMED read in uid.c) is gpio 6.
 * Known lines are named via DT `gpio-line-names`; everything else is a raw bit.
 *
 * CONFIRM-ON-HARDWARE: the firmware writes CPLD bits through the `iod_cpld`
 * INTEGRITY IODevice (a locking wrapper over this same MMIO), so a plain RMW
 * here should drive the pins but may race the CPLD's own updates on shared
 * registers — add a hardware mutex if that proves necessary. Only the UID read
 * path (reg+0x00 bit6) is directly CONFIRMED; the power/PSU bit positions come
 * from docs/09's function/string labelling, not bit-exact disassembly.
 */

#include <linux/bitops.h>
#include <linux/gpio/driver.h>
#include <linux/io.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/spinlock.h>

struct gxe_cpld_gpio {
	struct gpio_chip	gc;
	void __iomem		*base;
	spinlock_t		lock;		/* serialise RMW on shared bytes */
	unsigned long		*dir_out;	/* 1 = output (bitmap, ngpio bits) */
};

static inline void gxe_split(unsigned int off, unsigned int *reg, u8 *mask)
{
	*reg  = off >> 3;
	*mask = BIT(off & 7);
}

static int gxe_cpld_get(struct gpio_chip *gc, unsigned int off)
{
	struct gxe_cpld_gpio *g = gpiochip_get_data(gc);
	unsigned int reg;
	u8 mask;

	gxe_split(off, &reg, &mask);
	return !!(readb(g->base + reg) & mask);
}

static void gxe_cpld_set(struct gpio_chip *gc, unsigned int off, int val)
{
	struct gxe_cpld_gpio *g = gpiochip_get_data(gc);
	unsigned long flags;
	unsigned int reg;
	u8 mask, v;

	gxe_split(off, &reg, &mask);
	spin_lock_irqsave(&g->lock, flags);
	v = readb(g->base + reg);
	if (val)
		v |= mask;
	else
		v &= ~mask;
	writeb(v, g->base + reg);
	spin_unlock_irqrestore(&g->lock, flags);
}

static int gxe_cpld_get_dir(struct gpio_chip *gc, unsigned int off)
{
	struct gxe_cpld_gpio *g = gpiochip_get_data(gc);

	return test_bit(off, g->dir_out) ? GPIO_LINE_DIRECTION_OUT
					 : GPIO_LINE_DIRECTION_IN;
}

static int gxe_cpld_dir_in(struct gpio_chip *gc, unsigned int off)
{
	struct gxe_cpld_gpio *g = gpiochip_get_data(gc);

	clear_bit(off, g->dir_out);		/* register has no DDR; track in sw */
	return 0;
}

static int gxe_cpld_dir_out(struct gpio_chip *gc, unsigned int off, int val)
{
	struct gxe_cpld_gpio *g = gpiochip_get_data(gc);

	gxe_cpld_set(gc, off, val);
	set_bit(off, g->dir_out);
	return 0;
}

static int gxe_cpld_gpio_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct gxe_cpld_gpio *g;
	struct resource *res;
	unsigned int ngpio;

	g = devm_kzalloc(dev, sizeof(*g), GFP_KERNEL);
	if (!g)
		return -ENOMEM;

	g->base = devm_platform_get_and_ioremap_resource(pdev, 0, &res);
	if (IS_ERR(g->base))
		return PTR_ERR(g->base);

	ngpio = resource_size(res) * 8;		/* one GPIO per register bit */
	g->dir_out = devm_bitmap_zalloc(dev, ngpio, GFP_KERNEL);
	if (!g->dir_out)
		return -ENOMEM;

	spin_lock_init(&g->lock);

	g->gc.label		= "gxe-cpld-gpio";
	g->gc.parent		= dev;
	g->gc.owner		= THIS_MODULE;
	g->gc.base		= -1;
	g->gc.ngpio		= ngpio;
	g->gc.can_sleep		= false;
	g->gc.get		= gxe_cpld_get;
	g->gc.set		= gxe_cpld_set;
	g->gc.get_direction	= gxe_cpld_get_dir;
	g->gc.direction_input	= gxe_cpld_dir_in;
	g->gc.direction_output	= gxe_cpld_dir_out;

	return devm_gpiochip_add_data(dev, &g->gc, g);
}

static const struct of_device_id gxe_cpld_gpio_of_match[] = {
	{ .compatible = "hpe,gxe-cpld-gpio" },
	{ }
};
MODULE_DEVICE_TABLE(of, gxe_cpld_gpio_of_match);

static struct platform_driver gxe_cpld_gpio_driver = {
	.probe	= gxe_cpld_gpio_probe,
	.driver	= {
		.name		= "gxe-cpld-gpio",
		.of_match_table	= gxe_cpld_gpio_of_match,
	},
};
module_platform_driver(gxe_cpld_gpio_driver);

MODULE_DESCRIPTION("HP iLO 3 Gromit-XE CPLD GPIO driver");
MODULE_LICENSE("GPL");
