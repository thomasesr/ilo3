// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE (GXE) interrupt controller — root irqchip
 *
 * Reverse-engineered from the iLO 3 INTEGRITY kernel (see docs/12). The primary
 * ARM926 interrupt controller is two 32-line banks = 64 IRQs:
 *     bank0 (IRQ 0-31)  @ 0xCEFF0000
 *     bank1 (IRQ 32-63) @ 0x80F00000
 *
 * Per-bank registers (offset from bank base):
 *   +0x10  ENABLE   1 = line enabled               (CONFIRMED: fw mask fn 0x3404)
 *   +0x14  DISABLE  1 = line masked (= ~ENABLE);    (CONFIRMED: init writes
 *                   fw keeps the two complementary;  0xFFFFFFFF here = all masked)
 *   +0x0C  PENDING  raw/pending interrupt status     (read; docs/12)
 *   +0x30  EOI      write 1<<line to clear pending   (docs/12; fw RMWs it)
 *   +0x100[32] per-source config (priority/route)   (not used here)
 *
 * The ENABLE/DISABLE pair (+0x10/+0x14) is solid; PENDING(+0x0C)/EOI(+0x30) are
 * the best reading of the RE — confirm the exact offsets on hardware.
 *
 * The fw writes ENABLE(+0x10)=mask and DISABLE(+0x14)=~mask on every change, so
 * this driver mirrors that exactly. STATUS/ack is the one educated guess.
 */

#define pr_fmt(fmt) "gxe-intc: " fmt

#include <linux/io.h>
#include <linux/irq.h>
#include <linux/irqchip.h>
#include <linux/irqchip/chained_irq.h>
#include <linux/irqdomain.h>
#include <linux/of_address.h>
#include <linux/spinlock.h>

#define GXE_INTC_PENDING	0x0C	/* read: raw/pending status (docs/12) */
#define GXE_INTC_ENABLE		0x10	/* 1 = enabled   (CONFIRMED, fw 0x3404) */
#define GXE_INTC_DISABLE	0x14	/* 1 = masked=~ENABLE (CONFIRMED, init) */
#define GXE_INTC_EOI		0x30	/* write 1<<line to clear (docs/12) */

#define GXE_INTC_BANKS		2
#define GXE_INTC_PER_BANK	32
#define GXE_INTC_NR_IRQS	(GXE_INTC_BANKS * GXE_INTC_PER_BANK)

struct gxe_intc {
	void __iomem		*base[GXE_INTC_BANKS];
	u32			enabled[GXE_INTC_BANKS];	/* enable-reg shadow */
	struct irq_domain	*domain;
	raw_spinlock_t		lock;
};

static struct gxe_intc *gxe_intc;

/* push the shadow to hardware: ENABLE = shadow, DISABLE = ~shadow (matches fw) */
static void gxe_intc_sync(struct gxe_intc *ic, unsigned int bank)
{
	writel_relaxed(ic->enabled[bank],  ic->base[bank] + GXE_INTC_ENABLE);
	writel_relaxed(~ic->enabled[bank], ic->base[bank] + GXE_INTC_DISABLE);
}

static void gxe_intc_mask(struct irq_data *d)
{
	struct gxe_intc *ic = irq_data_get_irq_chip_data(d);
	unsigned int bank = d->hwirq / GXE_INTC_PER_BANK;
	unsigned int line = d->hwirq % GXE_INTC_PER_BANK;
	unsigned long flags;

	raw_spin_lock_irqsave(&ic->lock, flags);
	ic->enabled[bank] &= ~BIT(line);
	gxe_intc_sync(ic, bank);
	raw_spin_unlock_irqrestore(&ic->lock, flags);
}

static void gxe_intc_unmask(struct irq_data *d)
{
	struct gxe_intc *ic = irq_data_get_irq_chip_data(d);
	unsigned int bank = d->hwirq / GXE_INTC_PER_BANK;
	unsigned int line = d->hwirq % GXE_INTC_PER_BANK;
	unsigned long flags;

	raw_spin_lock_irqsave(&ic->lock, flags);
	ic->enabled[bank] |= BIT(line);
	gxe_intc_sync(ic, bank);
	raw_spin_unlock_irqrestore(&ic->lock, flags);
}

/* write-1-to-clear the pending bit (assumed; see header) */
static void gxe_intc_eoi(struct irq_data *d)
{
	struct gxe_intc *ic = irq_data_get_irq_chip_data(d);
	unsigned int bank = d->hwirq / GXE_INTC_PER_BANK;
	unsigned int line = d->hwirq % GXE_INTC_PER_BANK;

	writel_relaxed(BIT(line), ic->base[bank] + GXE_INTC_EOI);
}

static struct irq_chip gxe_intc_chip = {
	.name		= "GXE-INTC",
	.irq_mask	= gxe_intc_mask,
	.irq_unmask	= gxe_intc_unmask,
	.irq_eoi	= gxe_intc_eoi,
	.flags		= IRQCHIP_SKIP_SET_WAKE,
};

/* Root flow handler: ARM IRQ vector lands here (set_handle_irq below). */
static void __exception_irq_entry gxe_handle_irq(struct pt_regs *regs)
{
	struct gxe_intc *ic = gxe_intc;
	unsigned int bank;

	for (bank = 0; bank < GXE_INTC_BANKS; bank++) {
		u32 stat = readl_relaxed(ic->base[bank] + GXE_INTC_PENDING) &
			   ic->enabled[bank];

		while (stat) {
			unsigned int line = __ffs(stat);

			generic_handle_domain_irq(ic->domain,
						  bank * GXE_INTC_PER_BANK + line);
			stat &= ~BIT(line);
		}
	}
}

static int gxe_intc_domain_map(struct irq_domain *d, unsigned int virq,
			       irq_hw_number_t hw)
{
	irq_set_chip_and_handler(virq, &gxe_intc_chip, handle_fasteoi_irq);
	irq_set_chip_data(virq, d->host_data);
	irq_set_probe(virq);
	return 0;
}

static const struct irq_domain_ops gxe_intc_domain_ops = {
	.xlate	= irq_domain_xlate_onecell,	/* interrupts = <line> */
	.map	= gxe_intc_domain_map,
};

static int __init gxe_intc_of_init(struct device_node *node,
				   struct device_node *parent)
{
	struct gxe_intc *ic;
	unsigned int i;
	int ret;

	ic = kzalloc(sizeof(*ic), GFP_KERNEL);
	if (!ic)
		return -ENOMEM;
	raw_spin_lock_init(&ic->lock);

	for (i = 0; i < GXE_INTC_BANKS; i++) {
		ic->base[i] = of_iomap(node, i);
		if (!ic->base[i]) {
			pr_err("failed to map bank %u\n", i);
			ret = -ENXIO;
			goto err_unmap;
		}
		ic->enabled[i] = 0;
		/* mask everything, clear any latched pending */
		writel_relaxed(0, ic->base[i] + GXE_INTC_ENABLE);
		writel_relaxed(~0U, ic->base[i] + GXE_INTC_DISABLE);
		writel_relaxed(~0U, ic->base[i] + GXE_INTC_EOI);
	}

	ic->domain = irq_domain_add_linear(node, GXE_INTC_NR_IRQS,
					   &gxe_intc_domain_ops, ic);
	if (!ic->domain) {
		ret = -ENOMEM;
		goto err_unmap;
	}

	gxe_intc = ic;
	set_handle_irq(gxe_handle_irq);
	pr_info("HP iLO3 Gromit-XE: 2 banks, %d IRQs\n", GXE_INTC_NR_IRQS);
	return 0;

err_unmap:
	while (i--)
		iounmap(ic->base[i]);
	kfree(ic);
	return ret;
}

IRQCHIP_DECLARE(gxe_intc, "hpe,gxe-intc", gxe_intc_of_init);
