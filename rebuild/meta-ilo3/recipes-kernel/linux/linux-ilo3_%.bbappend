# HP iLO3 Gromit-XE: RE-derived devicetree + new drivers (irqchip, timer, UART, I2C, SPI-NOR, CPLD-gpio, PECI, fan-hwmon, GXELAN-MAC).
# These Gromit-XE blocks have no upstream drivers; this installs the sources,
# splices them into the kernel Kconfig/Makefile, and applies a config fragment.
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:${THISDIR}/files:"

SRC_URI:append:ilo3-gxe = " \
    file://ilo3-gxe.dts \
    file://irq-gxe.c \
    file://timer-gxe.c \
    file://gxe-uart.c \
    file://i2c-gxe.c \
    file://spi-gxe-smif.c \
    file://gpio-gxe-cpld.c \
    file://peci-gxe.c \
    file://fan-gxe.c \
    file://gxe-gmac.c \
    file://gxe.cfg \
"

do_configure:prepend:ilo3-gxe() {
    # --- devicetree ---
    install -d ${S}/arch/arm/boot/dts/hpe
    install -m 0644 ${WORKDIR}/ilo3-gxe.dts ${S}/arch/arm/boot/dts/hpe/ilo3-gxe.dts || \
    install -m 0644 ${WORKDIR}/ilo3-gxe.dts ${S}/arch/arm/boot/dts/ilo3-gxe.dts

    # --- irqchip: drivers/irqchip/irq-gxe.c ---
    install -m 0644 ${WORKDIR}/irq-gxe.c ${S}/drivers/irqchip/irq-gxe.c
    if ! grep -q GXE_IRQ ${S}/drivers/irqchip/Kconfig; then
        sed -i '/^endmenu/i \
config GXE_IRQ\n\tbool "HP iLO3 Gromit-XE interrupt controller"\n\tselect IRQ_DOMAIN\n' \
            ${S}/drivers/irqchip/Kconfig
    fi
    grep -q 'irq-gxe.o' ${S}/drivers/irqchip/Makefile || \
        echo 'obj-$(CONFIG_GXE_IRQ)	+= irq-gxe.o' >> ${S}/drivers/irqchip/Makefile

    # --- timer: drivers/clocksource/timer-gxe.c (clocksource only) ---
    install -m 0644 ${WORKDIR}/timer-gxe.c ${S}/drivers/clocksource/timer-gxe.c
    if ! grep -q GXE_TIMER ${S}/drivers/clocksource/Kconfig; then
        sed -i '/^endmenu/i \
config GXE_TIMER\n\tbool "HP iLO3 Gromit-XE timer (clocksource)"\n\tselect TIMER_OF\n' \
            ${S}/drivers/clocksource/Kconfig
    fi
    grep -q 'timer-gxe.o' ${S}/drivers/clocksource/Makefile || \
        echo 'obj-$(CONFIG_GXE_TIMER)	+= timer-gxe.o' >> ${S}/drivers/clocksource/Makefile

    # --- UART: drivers/tty/serial/gxe-uart.c ---
    install -m 0644 ${WORKDIR}/gxe-uart.c ${S}/drivers/tty/serial/gxe-uart.c
    if ! grep -q SERIAL_GXE ${S}/drivers/tty/serial/Kconfig; then
        sed -i '/^endmenu/i \
config SERIAL_GXE\n\ttristate "HP iLO3 Gromit-XE UART support"\n\tdepends on OF\n\tselect SERIAL_CORE\n\nconfig SERIAL_GXE_CONSOLE\n\tbool "Console on Gromit-XE UART"\n\tdepends on SERIAL_GXE=y\n\tselect SERIAL_CORE_CONSOLE\n\tselect SERIAL_EARLYCON\n' \
            ${S}/drivers/tty/serial/Kconfig
    fi
    grep -q 'gxe-uart.o' ${S}/drivers/tty/serial/Makefile || \
        echo 'obj-$(CONFIG_SERIAL_GXE)	+= gxe-uart.o' >> ${S}/drivers/tty/serial/Makefile
    # --- I2C: drivers/i2c/busses/i2c-gxe.c (8 Gromit-XE engines) ---
    install -m 0644 ${WORKDIR}/i2c-gxe.c ${S}/drivers/i2c/busses/i2c-gxe.c
    if ! grep -q I2C_GXE ${S}/drivers/i2c/busses/Kconfig; then
        sed -i '/^endmenu/i \
config I2C_GXE\n\ttristate "HP iLO3 Gromit-XE I2C"\n\tdepends on OF\n' \
            ${S}/drivers/i2c/busses/Kconfig
    fi
    grep -q 'i2c-gxe.o' ${S}/drivers/i2c/busses/Makefile || \
        echo 'obj-$(CONFIG_I2C_GXE)\t+= i2c-gxe.o' >> ${S}/drivers/i2c/busses/Makefile
    # --- SPI-NOR: drivers/spi/spi-gxe-smif.c (iLO boot flash controller) ---
    install -m 0644 ${WORKDIR}/spi-gxe-smif.c ${S}/drivers/spi/spi-gxe-smif.c
    if ! grep -q SPI_GXE_SMIF ${S}/drivers/spi/Kconfig; then
        sed -i '/^endif/i \
config SPI_GXE_SMIF\n\ttristate "HP iLO3 Gromit-XE SMIF SPI-NOR"\n\tdepends on OF\n\tselect SPI_MEM\n' \
            ${S}/drivers/spi/Kconfig
    fi
    grep -q 'spi-gxe-smif.o' ${S}/drivers/spi/Makefile || \
        echo 'obj-$(CONFIG_SPI_GXE_SMIF)\t+= spi-gxe-smif.o' >> ${S}/drivers/spi/Makefile
    # --- GPIO: drivers/gpio/gpio-gxe-cpld.c (system CPLD discrete lines) ---
    install -m 0644 ${WORKDIR}/gpio-gxe-cpld.c ${S}/drivers/gpio/gpio-gxe-cpld.c
    if ! grep -q GPIO_GXE_CPLD ${S}/drivers/gpio/Kconfig; then
        sed -i '/^endmenu/i \
config GPIO_GXE_CPLD\n\ttristate "HP iLO3 Gromit-XE CPLD GPIO"\n\tdepends on OF\n\tselect GPIOLIB\n' \
            ${S}/drivers/gpio/Kconfig
    fi
    grep -q 'gpio-gxe-cpld.o' ${S}/drivers/gpio/Makefile || \
        echo 'obj-$(CONFIG_GPIO_GXE_CPLD)\t+= gpio-gxe-cpld.o' >> ${S}/drivers/gpio/Makefile
    # --- PECI: drivers/peci/controller/peci-gxe.c (CPU temp/config engine) ---
    install -m 0644 ${WORKDIR}/peci-gxe.c ${S}/drivers/peci/controller/peci-gxe.c
    if ! grep -q PECI_GXE ${S}/drivers/peci/controller/Kconfig; then
        sed -i '/^endif/i \
config PECI_GXE\n\ttristate "HP iLO3 Gromit-XE PECI"\n\tdepends on OF\n' \
            ${S}/drivers/peci/controller/Kconfig
    fi
    grep -q 'peci-gxe.o' ${S}/drivers/peci/controller/Makefile || \
        echo 'obj-$(CONFIG_PECI_GXE)\t+= peci-gxe.o' >> ${S}/drivers/peci/controller/Makefile
    # --- HWMON: drivers/hwmon/fan-gxe.c (CPLD fan scan-chain PWM/tach) ---
    install -m 0644 ${WORKDIR}/fan-gxe.c ${S}/drivers/hwmon/fan-gxe.c
    if ! grep -q SENSORS_GXE_FAN ${S}/drivers/hwmon/Kconfig; then
        sed -i '/^endif/i \
config SENSORS_GXE_FAN\n\ttristate "HP iLO3 Gromit-XE fan scan-chain"\n\tdepends on OF\n' \
            ${S}/drivers/hwmon/Kconfig
    fi
    grep -q 'fan-gxe.o' ${S}/drivers/hwmon/Makefile || \
        echo 'obj-$(CONFIG_SENSORS_GXE_FAN)\t+= fan-gxe.o' >> ${S}/drivers/hwmon/Makefile
    # --- NET: drivers/net/ethernet/gxe-gmac.c (GXELAN Ethernet MAC + MDIO) ---
    install -m 0644 ${WORKDIR}/gxe-gmac.c ${S}/drivers/net/ethernet/gxe-gmac.c
    if ! grep -q GXE_GMAC ${S}/drivers/net/ethernet/Kconfig; then
        sed -i '/^endif/i \
config GXE_GMAC\n\ttristate "HP iLO3 Gromit-XE GXELAN Ethernet"\n\tdepends on OF\n\tselect PHYLIB\n' \
            ${S}/drivers/net/ethernet/Kconfig
    fi
    grep -q 'gxe-gmac.o' ${S}/drivers/net/ethernet/Makefile || \
        echo 'obj-$(CONFIG_GXE_GMAC)\t+= gxe-gmac.o' >> ${S}/drivers/net/ethernet/Makefile
}

KERNEL_FEATURES:append:ilo3-gxe = " gxe.cfg"
