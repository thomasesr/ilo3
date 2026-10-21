// SPDX-License-Identifier: GPL-2.0
/*
 * HP iLO 3 Gromit-XE (GXE) UART driver
 *
 * A minimal 8-bit polled UART in the Gromit-XE ASIC core block. Recovered by
 * reverse-engineering iLO 3 firmware (see docs/05): the boot ROM putchar/getchar
 * use only two 8-bit registers — a data register and a status register with
 * TX-ready (bit5) and RX-ready (bit0) flags. No baud/line-control registers are
 * exposed (rate is strap/clock-configured), so set_termios is a near no-op and
 * there is no TX/RX interrupt (DT describes it as polled).
 *
 *   data   @ reg+0x00   write = THR, read = RBR
 *   status @ reg+0x05   bit5 (0x20) = TX holding empty, bit0 (0x01) = RX ready
 */

#include <linux/console.h>
#include <linux/io.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/serial_core.h>
#include <linux/tty_flip.h>

#define GXE_UART_DATA		0x00
#define GXE_UART_STATUS		0x05
#define GXE_UART_TXRDY		0x20	/* TX holding register empty */
#define GXE_UART_RXRDY		0x01	/* RX data available */

#define GXE_UART_NR		1
#define GXE_UART_DRV_NAME	"gxe-uart"
#define GXE_UART_DEV_NAME	"ttyGXE"

/* Upstreaming this driver should add PORT_GXE to uapi/linux/serial_core.h and
 * drop this local define. Use a high, currently-unused value for now. */
#ifndef PORT_GXE
#define PORT_GXE		123
#endif

static inline u8 gxe_status(struct uart_port *port)
{
	return readb(port->membase + GXE_UART_STATUS);
}

/* Spin until the TX holding register is empty, then push one byte. */
static void gxe_putc(struct uart_port *port, u8 ch)
{
	while (!(gxe_status(port) & GXE_UART_TXRDY))
		cpu_relax();
	writeb(ch, port->membase + GXE_UART_DATA);
}

/* ---- earlycon: the whole point — first console output on new hardware ---- */

static void gxe_earlycon_write(struct console *con, const char *s, unsigned n)
{
	struct earlycon_device *dev = con->data;

	uart_console_write(&dev->port, s, n, gxe_putc);
}

static int __init gxe_earlycon_setup(struct earlycon_device *dev,
				     const char *opt)
{
	if (!dev->port.membase)
		return -ENODEV;
	dev->con->write = gxe_earlycon_write;
	return 0;
}
OF_EARLYCON_DECLARE(gxe_uart, "hpe,gxe-uart", gxe_earlycon_setup);

/* ---- full tty serial driver (polled; no TX/RX interrupt in hardware) ---- */

struct gxe_port {
	struct uart_port port;
	struct timer_list rx_timer;	/* polls RX since there is no IRQ */
};

#define to_gxe(p) container_of(p, struct gxe_port, port)
#define GXE_RX_POLL_MS 8

static void gxe_rx_poll(struct timer_list *t)
{
	struct gxe_port *gp = from_timer(gp, t, rx_timer);
	struct uart_port *port = &gp->port;
	unsigned long flags;
	int pushed = 0;

	spin_lock_irqsave(&port->lock, flags);
	while (gxe_status(port) & GXE_UART_RXRDY) {
		u8 ch = readb(port->membase + GXE_UART_DATA);

		port->icount.rx++;
		if (uart_handle_sysrq_char(port, ch))
			continue;
		uart_insert_char(port, 0, 0, ch, TTY_NORMAL);
		pushed = 1;
	}
	spin_unlock_irqrestore(&port->lock, flags);

	if (pushed)
		tty_flip_buffer_push(&port->state->port);
	mod_timer(&gp->rx_timer, jiffies + msecs_to_jiffies(GXE_RX_POLL_MS));
}

static unsigned int gxe_tx_empty(struct uart_port *port)
{
	return (gxe_status(port) & GXE_UART_TXRDY) ? TIOCSER_TEMT : 0;
}

/* No modem-control lines on this UART — report always-asserted. */
static unsigned int gxe_get_mctrl(struct uart_port *port)
{
	return TIOCM_CTS | TIOCM_DSR | TIOCM_CAR;
}
static void gxe_set_mctrl(struct uart_port *port, unsigned int mctrl) { }
static void gxe_stop_tx(struct uart_port *port) { }
static void gxe_stop_rx(struct uart_port *port) { }
static void gxe_break_ctl(struct uart_port *port, int ctl) { }

static void gxe_start_tx(struct uart_port *port)
{
	u8 ch;

	uart_port_tx(port, ch,
		     gxe_status(port) & GXE_UART_TXRDY,
		     writeb(ch, port->membase + GXE_UART_DATA));
}

static int gxe_startup(struct uart_port *port)
{
	struct gxe_port *gp = to_gxe(port);

	timer_setup(&gp->rx_timer, gxe_rx_poll, 0);
	mod_timer(&gp->rx_timer, jiffies + msecs_to_jiffies(GXE_RX_POLL_MS));
	return 0;
}

static void gxe_shutdown(struct uart_port *port)
{
	struct gxe_port *gp = to_gxe(port);

	del_timer_sync(&gp->rx_timer);
}

/* Fixed-format, strap-configured line — nothing to program. */
static void gxe_set_termios(struct uart_port *port, struct ktermios *termios,
			    const struct ktermios *old)
{
	unsigned int baud = uart_get_baud_rate(port, termios, old, 0, 115200);
	unsigned long flags;

	termios->c_cflag &= ~(CSIZE | PARENB | CSTOPB);
	termios->c_cflag |= CS8;			/* 8N1 only */
	spin_lock_irqsave(&port->lock, flags);
	uart_update_timeout(port, termios->c_cflag, baud);
	spin_unlock_irqrestore(&port->lock, flags);
}

static const char *gxe_type(struct uart_port *port)
{
	return port->type == PORT_GXE ? "GXE-UART" : NULL;
}
static int gxe_request_port(struct uart_port *port) { return 0; }
static void gxe_release_port(struct uart_port *port) { }
static void gxe_config_port(struct uart_port *port, int flags)
{
	if (flags & UART_CONFIG_TYPE)
		port->type = PORT_GXE;
}

static const struct uart_ops gxe_uart_ops = {
	.tx_empty	= gxe_tx_empty,
	.set_mctrl	= gxe_set_mctrl,
	.get_mctrl	= gxe_get_mctrl,
	.stop_tx	= gxe_stop_tx,
	.start_tx	= gxe_start_tx,
	.stop_rx	= gxe_stop_rx,
	.break_ctl	= gxe_break_ctl,
	.startup	= gxe_startup,
	.shutdown	= gxe_shutdown,
	.set_termios	= gxe_set_termios,
	.type		= gxe_type,
	.request_port	= gxe_request_port,
	.release_port	= gxe_release_port,
	.config_port	= gxe_config_port,
};

/* ---- console (so printk reaches the port) ---- */

static struct gxe_port *gxe_ports[GXE_UART_NR];
static struct uart_driver gxe_uart_driver;

static void gxe_console_write(struct console *co, const char *s, unsigned count)
{
	struct uart_port *port = &gxe_ports[co->index]->port;
	unsigned long flags;
	int locked = 1;

	if (oops_in_progress)
		locked = spin_trylock_irqsave(&port->lock, flags);
	else
		spin_lock_irqsave(&port->lock, flags);

	uart_console_write(port, s, count, gxe_putc);

	if (locked)
		spin_unlock_irqrestore(&port->lock, flags);
}

static int gxe_console_setup(struct console *co, char *options)
{
	struct uart_port *port;
	int baud = 9600, bits = 8, parity = 'n', flow = 'n';

	if (co->index < 0 || co->index >= GXE_UART_NR || !gxe_ports[co->index])
		return -ENODEV;
	port = &gxe_ports[co->index]->port;
	if (options)
		uart_parse_options(options, &baud, &parity, &bits, &flow);
	return uart_set_options(port, co, baud, parity, bits, flow);
}

static struct console gxe_console = {
	.name	= GXE_UART_DEV_NAME,
	.write	= gxe_console_write,
	.device	= uart_console_device,
	.setup	= gxe_console_setup,
	.flags	= CON_PRINTBUFFER,
	.index	= -1,
	.data	= &gxe_uart_driver,
};

static struct uart_driver gxe_uart_driver = {
	.owner		= THIS_MODULE,
	.driver_name	= GXE_UART_DRV_NAME,
	.dev_name	= GXE_UART_DEV_NAME,
	.nr		= GXE_UART_NR,
	.cons		= &gxe_console,
};

/* ---- platform driver ---- */

static int gxe_uart_probe(struct platform_device *pdev)
{
	struct gxe_port *gp;
	struct resource *res;
	int ret, line = 0;

	gp = devm_kzalloc(&pdev->dev, sizeof(*gp), GFP_KERNEL);
	if (!gp)
		return -ENOMEM;

	gp->port.membase = devm_platform_get_and_ioremap_resource(pdev, 0, &res);
	if (IS_ERR(gp->port.membase))
		return PTR_ERR(gp->port.membase);

	gp->port.mapbase	= res->start;
	gp->port.dev		= &pdev->dev;
	gp->port.type		= PORT_GXE;
	gp->port.iotype		= UPIO_MEM;
	gp->port.fifosize	= 1;
	gp->port.ops		= &gxe_uart_ops;
	gp->port.flags		= UPF_BOOT_AUTOCONF;
	gp->port.line		= line;
	spin_lock_init(&gp->port.lock);

	gxe_ports[line] = gp;
	platform_set_drvdata(pdev, gp);

	ret = uart_add_one_port(&gxe_uart_driver, &gp->port);
	if (ret)
		gxe_ports[line] = NULL;
	return ret;
}

static int gxe_uart_remove(struct platform_device *pdev)
{
	struct gxe_port *gp = platform_get_drvdata(pdev);

	uart_remove_one_port(&gxe_uart_driver, &gp->port);
	gxe_ports[gp->port.line] = NULL;
	return 0;
}

static const struct of_device_id gxe_uart_of_match[] = {
	{ .compatible = "hpe,gxe-uart" },
	{ }
};
MODULE_DEVICE_TABLE(of, gxe_uart_of_match);

static struct platform_driver gxe_uart_platform_driver = {
	.probe	= gxe_uart_probe,
	.remove	= gxe_uart_remove,
	.driver	= {
		.name		= GXE_UART_DRV_NAME,
		.of_match_table	= gxe_uart_of_match,
	},
};

static int __init gxe_uart_init(void)
{
	int ret = uart_register_driver(&gxe_uart_driver);

	if (ret)
		return ret;
	ret = platform_driver_register(&gxe_uart_platform_driver);
	if (ret)
		uart_unregister_driver(&gxe_uart_driver);
	return ret;
}

static void __exit gxe_uart_exit(void)
{
	platform_driver_unregister(&gxe_uart_platform_driver);
	uart_unregister_driver(&gxe_uart_driver);
}

module_init(gxe_uart_init);
module_exit(gxe_uart_exit);

MODULE_DESCRIPTION("HP iLO 3 Gromit-XE UART driver");
MODULE_LICENSE("GPL");
