// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE (GXE) timer — clocksource + sched_clock
 *
 * Reverse-engineered from the iLO 3 INTEGRITY kernel (docs/05,12). The ASIC has
 * a free-running 64-bit up-counter in the core block at 0xC0000098, read by the
 * firmware with a single `ldrd` (low @+0x00, high @+0x04). This gives Linux a
 * monotonic clocksource and sched_clock.
 *
 *   COUNTER_LO @ reg+0x00  (0xC0000098)
 *   COUNTER_HI @ reg+0x04  (0xC000009C)        [CONFIRMED]
 *
 * Counter frequency is a hardware parameter not visible in the firmware; it is
 * taken from the DT `clock-frequency` (fill from hardware/datasheet).
 *
 * CLOCKEVENT ("hpe,gxe-wallace" node) uses the separate periodic timer in the
 * "Wallace" companion block @0xD1000000 (IRQ 36). CONFIRMED from the firmware:
 *   CTRL   @+0x09  write 0x80 = enable         (calib code 0x35f0/0x3944)
 *   STATUS @+0x00  bit7 (0x80) = tick pending   (tick handler 0x1acc4)
 * BEST-GUESS (flagged GXE_CE_* below — CONFIRM ON HARDWARE): the reload/period
 * register offset and the exact ack. Static RE could not pin these: the fw uses
 * the free-running counter for busy-wait delays and routes the OS tick through
 * an abstracted clock object. Fix the two #defines once measured on hardware.
 */

#include <linux/clockchips.h>
#include <linux/clocksource.h>
#include <linux/interrupt.h>
#include <linux/of.h>
#include <linux/of_address.h>
#include <linux/of_irq.h>
#include <linux/sched_clock.h>

#define GXE_TIMER_CNT_LO	0x00
#define GXE_TIMER_CNT_HI	0x04

static void __iomem *gxe_timer_base;

/* 64-bit read of a free-running counter across a 32-bit bus (rollover-safe). */
static u64 gxe_timer_read(void)
{
	u32 hi, lo;

	do {
		hi = readl_relaxed(gxe_timer_base + GXE_TIMER_CNT_HI);
		lo = readl_relaxed(gxe_timer_base + GXE_TIMER_CNT_LO);
	} while (hi != readl_relaxed(gxe_timer_base + GXE_TIMER_CNT_HI));

	return ((u64)hi << 32) | lo;
}

static u64 gxe_clocksource_read(struct clocksource *cs)
{
	return gxe_timer_read();
}

static struct clocksource gxe_clocksource = {
	.name	= "gxe-timer",
	.rating	= 300,
	.read	= gxe_clocksource_read,
	.mask	= CLOCKSOURCE_MASK(64),
	.flags	= CLOCK_SOURCE_IS_CONTINUOUS,
};

static u64 notrace gxe_sched_clock_read(void)
{
	return gxe_timer_read();
}

static int __init gxe_timer_init(struct device_node *node)
{
	u32 freq;
	int ret;

	gxe_timer_base = of_iomap(node, 0);
	if (!gxe_timer_base) {
		pr_err("gxe-timer: failed to map counter\n");
		return -ENXIO;
	}

	ret = of_property_read_u32(node, "clock-frequency", &freq);
	if (ret) {
		pr_err("gxe-timer: missing clock-frequency\n");
		ret = -EINVAL;
		goto err;
	}

	ret = clocksource_register_hz(&gxe_clocksource, freq);
	if (ret) {
		pr_err("gxe-timer: clocksource register failed\n");
		goto err;
	}
	sched_clock_register(gxe_sched_clock_read, 64, freq);

	pr_info("gxe-timer: 64-bit clocksource @ %u Hz\n", freq);
	return 0;

err:
	iounmap(gxe_timer_base);
	gxe_timer_base = NULL;
	return ret;
}

TIMER_OF_DECLARE(gxe_timer, "hpe,gxe-timer", gxe_timer_init);

/* ============================ clockevent ============================ *
 * Periodic timer in the Wallace block (0xD1000000, IRQ 36).
 * CONFIRMED: CTRL +0x09 (write 0x80 = enable), STATUS +0x00 bit7 = pending.
 * CONFIRM-ON-HARDWARE: reload register offset + ack. Change these two if the
 * clockevent doesn't tick — everything else is standard.
 */
#define GXE_CE_STATUS		0x00		/* bit7 = pending (CONFIRMED) */
#define GXE_CE_RELOAD		0x04		/* down-counter reload (BEST GUESS) */
#define GXE_CE_CTRL		0x09		/* write ENABLE here (CONFIRMED) */
#define GXE_CE_PENDING		0x80		/* STATUS bit7 (CONFIRMED) */
#define GXE_CE_ENABLE		0x80		/* CTRL enable (CONFIRMED) */

struct gxe_clockevent {
	void __iomem		*base;
	struct clock_event_device ce;
	u32			period;		/* ticks per HZ (periodic) */
};

#define to_gxe_ce(e) container_of(e, struct gxe_clockevent, ce)

static inline void gxe_ce_arm(struct gxe_clockevent *gc, u32 ticks)
{
	writel_relaxed(ticks, gc->base + GXE_CE_RELOAD);
	writeb_relaxed(GXE_CE_ENABLE, gc->base + GXE_CE_CTRL);
}

static int gxe_ce_shutdown(struct clock_event_device *ce)
{
	struct gxe_clockevent *gc = to_gxe_ce(ce);

	writeb_relaxed(0, gc->base + GXE_CE_CTRL);
	return 0;
}

static int gxe_ce_set_periodic(struct clock_event_device *ce)
{
	struct gxe_clockevent *gc = to_gxe_ce(ce);

	gxe_ce_arm(gc, gc->period);
	return 0;
}

static int gxe_ce_set_oneshot(struct clock_event_device *ce)
{
	return gxe_ce_shutdown(ce);		/* armed later by set_next_event */
}

static int gxe_ce_set_next_event(unsigned long evt, struct clock_event_device *ce)
{
	gxe_ce_arm(to_gxe_ce(ce), evt);
	return 0;
}

static irqreturn_t gxe_ce_isr(int irq, void *dev_id)
{
	struct gxe_clockevent *gc = dev_id;

	if (!(readb_relaxed(gc->base + GXE_CE_STATUS) & GXE_CE_PENDING))
		return IRQ_NONE;

	/* ack: write-1-to-clear the pending bit (BEST GUESS) */
	writeb_relaxed(GXE_CE_PENDING, gc->base + GXE_CE_STATUS);

	if (clockevent_state_periodic(&gc->ce))
		gxe_ce_arm(gc, gc->period);	/* re-arm (works w/ or w/o auto-reload) */
	else
		gxe_ce_shutdown(&gc->ce);

	gc->ce.event_handler(&gc->ce);
	return IRQ_HANDLED;
}

static int __init gxe_clockevent_init(struct device_node *node)
{
	struct gxe_clockevent *gc;
	u32 freq;
	int irq, ret;

	gc = kzalloc(sizeof(*gc), GFP_KERNEL);
	if (!gc)
		return -ENOMEM;

	gc->base = of_iomap(node, 0);
	if (!gc->base) {
		ret = -ENXIO;
		goto err_free;
	}
	ret = of_property_read_u32(node, "clock-frequency", &freq);
	if (ret) {
		pr_err("gxe-timer: wallace missing clock-frequency\n");
		goto err_unmap;
	}
	gc->period = DIV_ROUND_CLOSEST(freq, HZ);

	irq = irq_of_parse_and_map(node, 0);
	if (!irq) {
		ret = -EINVAL;
		goto err_unmap;
	}

	writeb_relaxed(0, gc->base + GXE_CE_CTRL);	/* disable while setting up */

	gc->ce = (struct clock_event_device){
		.name			= "gxe-clockevent",
		.features		= CLOCK_EVT_FEAT_PERIODIC |
					  CLOCK_EVT_FEAT_ONESHOT,
		.rating			= 300,
		.set_state_shutdown	= gxe_ce_shutdown,
		.set_state_periodic	= gxe_ce_set_periodic,
		.set_state_oneshot	= gxe_ce_set_oneshot,
		.set_next_event		= gxe_ce_set_next_event,
		.cpumask		= cpu_possible_mask,
	};

	ret = request_irq(irq, gxe_ce_isr, IRQF_TIMER | IRQF_IRQPOLL,
			  "gxe-clockevent", gc);
	if (ret)
		goto err_unmap;

	clockevents_config_and_register(&gc->ce, freq, 1, 0xffffffff);
	pr_info("gxe-timer: clockevent (Wallace) @ %u Hz, IRQ %d\n", freq, irq);
	return 0;

err_unmap:
	iounmap(gc->base);
err_free:
	kfree(gc);
	return ret;
}

TIMER_OF_DECLARE(gxe_wallace, "hpe,gxe-wallace", gxe_clockevent_init);
