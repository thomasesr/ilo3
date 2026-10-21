// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE (GXE) SMIF SPI-NOR boot-flash controller
 *
 * Reverse-engineered from the iLO 3 INTEGRITY kernel `gxekflsh` driver
 * (decompiled/kernel_type2.bin, ARM:LE:32:v5; see docs/15,17). The iLO's own
 * boot flash (the `ilo3_194.bin` image) hangs off a register-file SPI master in
 * the ASIC core block at 0xC0000200 — NOT a memory-mapped XIP window. It is a
 * classic opcode/address/count/FIFO engine, so it is exposed to Linux as a
 * spi-mem controller and the spi-nor/mtd core handles the flash geometry and
 * partitioning.
 *
 * Register file (base 0xC0000200, all offsets CONFIRMED from fw disassembly):
 *   +0x00 u32  CTRL   — clear bits [13:11] (mask 0x3800) before each op (mode/clk)
 *   +0x02 u8   MODE   — 3 = data transfer (program/erase/read), 0 = no-data
 *   +0x04 u8   GO/BUSY— write launches the command phase; read bit1 = busy
 *   +0x05 u8   OPCODE — SPI command byte (unlock 0x55,0xAA written here first)
 *   +0x06 u8   COUNT  — data byte count (<= 0x40)
 *   +0x08 u32  ADDR   — 24-bit flash address
 *   +0x40 64B  FIFO   — program source / read dest / status result
 *
 * Confirmed opcodes/sequences from fw: RDSR 0x05, WREN 0x06, PP 0x02 (64-byte
 * FIFO chunks), BLOCK-ERASE 0xD8. Completion = poll GO/BUSY bit1 clear, then
 * (for erase/program) poll flash WIP via RDSR bit0.
 *
 * CONFIRM-ON-HARDWARE (flagged GXE_SMIF_GO_*): the exact GO-register encoding per
 * command class (fw used 1=RDSR, 9=WREN/erase, 0xD=program) and the read-data
 * path — the fw bulk-read route was not isolated; here reads reuse the same
 * opcode+addr+count+FIFO model (opcode 0x03), which the register file supports.
 */

#include <linux/delay.h>
#include <linux/io.h>
#include <linux/iopoll.h>
#include <linux/module.h>
#include <linux/mtd/spi-nor.h>
#include <linux/platform_device.h>
#include <linux/spi/spi.h>
#include <linux/spi/spi-mem.h>

#define GXE_SMIF_CTRL		0x00
#define GXE_SMIF_MODE		0x02
#define GXE_SMIF_GO		0x04
#define GXE_SMIF_OPCODE		0x05
#define GXE_SMIF_COUNT		0x06
#define GXE_SMIF_ADDR		0x08
#define GXE_SMIF_FIFO		0x40

#define GXE_SMIF_CTRL_MODEMASK	0x3800	/* bits [13:11] cleared before a transfer */
#define GXE_SMIF_MODE_DATA	0x03	/* MODE value for a data transfer */
#define GXE_SMIF_MODE_NONE	0x00	/* MODE value for a no-data command */
#define GXE_SMIF_BUSY		0x02	/* GO reg bit1 = controller busy */
#define GXE_SMIF_FIFO_SZ	0x40	/* 64-byte data window */

#define GXE_SMIF_SR_WIP		0x01	/* RDSR bit0 = flash write-in-progress */

/* GO-register launch values observed in firmware (CONFIRM-ON-HARDWARE) */
#define GXE_SMIF_GO_STATUS	0x01	/* RDSR */
#define GXE_SMIF_GO_CMD		0x09	/* WREN / erase / read (addr/no-data cmd) */
#define GXE_SMIF_GO_PROG	0x0D	/* page program */

struct gxe_smif {
	void __iomem	*base;
	struct device	*dev;
};

/* Unlock preamble: the fw writes 0x55 then 0xAA to the opcode reg before the
 * real opcode for control ops (RDSR/WREN). */
static void gxe_smif_unlock(struct gxe_smif *s)
{
	writeb_relaxed(0x55, s->base + GXE_SMIF_OPCODE);
	writeb_relaxed(0xAA, s->base + GXE_SMIF_OPCODE);
}

/* Clear the mode bits in CTRL ahead of a transfer (fw does this each op). */
static void gxe_smif_prep_ctrl(struct gxe_smif *s)
{
	u32 v = readl_relaxed(s->base + GXE_SMIF_CTRL);

	writel_relaxed(v & ~GXE_SMIF_CTRL_MODEMASK, s->base + GXE_SMIF_CTRL);
}

/* Wait for the controller command phase to finish (GO/BUSY bit1 clears). */
static int gxe_smif_wait_ready(struct gxe_smif *s)
{
	u8 v;

	return readb_relaxed_poll_timeout(s->base + GXE_SMIF_GO, v,
					  !(v & GXE_SMIF_BUSY), 1,
					  100 * USEC_PER_MSEC);
}

/*
 * Issue one SPI command through the register file, modelled on the fw sequence:
 *   CTRL mode-clear; [unlock]; MODE; ADDR; COUNT; FIFO<-tx; OPCODE; GO; wait.
 * @go selects the launch value (see GXE_SMIF_GO_* — CONFIRM-ON-HARDWARE).
 * data_out (tx!=NULL) is copied into the FIFO before launch; data_in (rx!=NULL)
 * is read back after. len must be <= 64.
 */
static int gxe_smif_run(struct gxe_smif *s, u8 opcode, bool has_addr, u32 addr,
			const u8 *tx, u8 *rx, unsigned int len, u8 go, bool unlock)
{
	unsigned int i;
	int ret;

	if (len > GXE_SMIF_FIFO_SZ)
		return -EINVAL;

	gxe_smif_prep_ctrl(s);
	if (unlock)
		gxe_smif_unlock(s);

	writeb_relaxed(len ? GXE_SMIF_MODE_DATA : GXE_SMIF_MODE_NONE,
		       s->base + GXE_SMIF_MODE);
	if (has_addr)
		writel_relaxed(addr & 0xffffff, s->base + GXE_SMIF_ADDR);
	writeb_relaxed(len, s->base + GXE_SMIF_COUNT);

	if (tx)
		for (i = 0; i < len; i++)
			writeb_relaxed(tx[i], s->base + GXE_SMIF_FIFO + i);

	writeb_relaxed(opcode, s->base + GXE_SMIF_OPCODE);
	writeb_relaxed(go,     s->base + GXE_SMIF_GO);		/* launch */

	ret = gxe_smif_wait_ready(s);
	if (ret)
		return ret;

	if (rx)
		for (i = 0; i < len; i++)
			rx[i] = readb_relaxed(s->base + GXE_SMIF_FIFO + i);

	return 0;
}

/* Read status register (opcode 0x05) → return WIP bit state. */
static int __maybe_unused gxe_smif_wip(struct gxe_smif *s)
{
	u8 sr = 0;
	int ret = gxe_smif_run(s, SPINOR_OP_RDSR, false, 0, NULL, &sr, 1,
			       GXE_SMIF_GO_STATUS, true);

	if (ret)
		return ret;
	return sr & GXE_SMIF_SR_WIP;
}

/* Wait for the flash internal op (erase/program) to complete. */
static int __maybe_unused gxe_smif_wait_wip(struct gxe_smif *s, unsigned int tmo_ms)
{
	unsigned long end = jiffies + msecs_to_jiffies(tmo_ms);
	int wip;

	do {
		wip = gxe_smif_wip(s);
		if (wip < 0)
			return wip;
		if (!wip)
			return 0;
		cond_resched();
	} while (time_before(jiffies, end));

	return -ETIMEDOUT;
}

/* ----------------------------- spi-mem ----------------------------- */

/* Choose the GO launch value for an op class (CONFIRM-ON-HARDWARE). */
static u8 gxe_smif_go_for(const struct spi_mem_op *op, bool has_addr)
{
	if (op->data.dir == SPI_MEM_DATA_OUT && has_addr)
		return GXE_SMIF_GO_PROG;			/* page program */
	if (!has_addr && op->data.nbytes && op->data.dir == SPI_MEM_DATA_IN)
		return GXE_SMIF_GO_STATUS;			/* RDSR / RDID */
	return GXE_SMIF_GO_CMD;				/* WREN / erase / read */
}

static int gxe_smif_exec_op(struct spi_mem *mem, const struct spi_mem_op *op)
{
	struct gxe_smif *s = spi_controller_get_devdata(mem->spi->controller);
	bool has_addr = op->addr.nbytes > 0;
	bool unlock = !has_addr;	/* fw unlocks the no-address control ops */
	unsigned int len = op->data.nbytes;
	u8 go = gxe_smif_go_for(op, has_addr);
	unsigned int off;
	int ret;

	/* no-data command (WREN, erase, ...) */
	if (!len)
		return gxe_smif_run(s, op->cmd.opcode, has_addr, op->addr.val,
				    NULL, NULL, 0, go, unlock);

	/* data phase — split into <=64-byte FIFO windows */
	for (off = 0; off < len; off += GXE_SMIF_FIFO_SZ) {
		unsigned int n = min_t(unsigned int, GXE_SMIF_FIFO_SZ, len - off);
		const u8 *tx = NULL;
		u8 *rx = NULL;

		if (op->data.dir == SPI_MEM_DATA_OUT)
			tx = op->data.buf.out + off;
		else
			rx = op->data.buf.in + off;

		ret = gxe_smif_run(s, op->cmd.opcode, has_addr, op->addr.val + off,
				   tx, rx, n, go, unlock && off == 0);
		if (ret)
			return ret;
	}
	return 0;
}

/* Cap each transfer to the 64-byte FIFO; the core splits larger reads/writes. */
static int gxe_smif_adjust_op_size(struct spi_mem *mem, struct spi_mem_op *op)
{
	if (op->data.nbytes > GXE_SMIF_FIFO_SZ)
		op->data.nbytes = GXE_SMIF_FIFO_SZ;
	return 0;
}

static bool gxe_smif_supports_op(struct spi_mem *mem,
				 const struct spi_mem_op *op)
{
	/* single-lane only; address is 3-byte; no dummy cycles modelled */
	if (op->cmd.buswidth > 1 || op->addr.buswidth > 1 ||
	    op->data.buswidth > 1)
		return false;
	if (op->addr.nbytes > 3)
		return false;
	return spi_mem_default_supports_op(mem, op);
}

static const struct spi_controller_mem_ops gxe_smif_mem_ops = {
	.adjust_op_size	= gxe_smif_adjust_op_size,
	.supports_op	= gxe_smif_supports_op,
	.exec_op	= gxe_smif_exec_op,
};

static int gxe_smif_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct spi_controller *ctlr;
	struct gxe_smif *s;
	int ret;

	ctlr = devm_spi_alloc_host(dev, sizeof(*s));
	if (!ctlr)
		return -ENOMEM;

	s = spi_controller_get_devdata(ctlr);
	s->dev = dev;
	s->base = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(s->base))
		return PTR_ERR(s->base);

	ctlr->mode_bits	 = 0;				/* single-lane SPI only */
	ctlr->bits_per_word_mask = SPI_BPW_MASK(8);
	ctlr->mem_ops	 = &gxe_smif_mem_ops;
	ctlr->dev.of_node = dev->of_node;
	ctlr->num_chipselect = 1;

	platform_set_drvdata(pdev, ctlr);

	ret = devm_spi_register_controller(dev, ctlr);
	if (ret)
		return dev_err_probe(dev, ret, "failed to register SMIF controller\n");

	dev_info(dev, "GXE SMIF SPI-NOR controller @ %pR\n",
		 platform_get_resource(pdev, IORESOURCE_MEM, 0));
	return 0;
}

static const struct of_device_id gxe_smif_of_match[] = {
	{ .compatible = "hpe,gxe-smif" },
	{ }
};
MODULE_DEVICE_TABLE(of, gxe_smif_of_match);

static struct platform_driver gxe_smif_driver = {
	.probe	= gxe_smif_probe,
	.driver	= {
		.name		= "gxe-smif",
		.of_match_table	= gxe_smif_of_match,
	},
};
module_platform_driver(gxe_smif_driver);

MODULE_DESCRIPTION("HP iLO 3 Gromit-XE SMIF SPI-NOR boot-flash controller");
MODULE_LICENSE("GPL");
