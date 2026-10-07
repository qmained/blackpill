#![no_std]
#![no_main]

mod display_task;
mod usb_task;

use core::cell::OnceCell;
use crate::display_task::update_display;
use crate::usb_task::{get_next_payload, PAYLOAD_SIZE};
use defmt::info;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::interrupt::typelevel::EXTI0;
use embassy_stm32::mode::Async;
use embassy_stm32::peripherals::{DMA1_CH5, DMA1_CH6, I2C1, USB_OTG_FS};
use embassy_stm32::time::Hertz;
use embassy_stm32::usb::Driver;
use embassy_stm32::{bind_interrupts, dma, exti, i2c, usb, Config};
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, ThreadModeRawMutex};
use embassy_sync::signal::Signal;
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::Builder;
use ssd1306::mode::{BufferedGraphicsModeAsync, DisplayConfigAsync};
use ssd1306::prelude::I2CInterface;
use ssd1306::rotation::DisplayRotation::Rotate0;
use ssd1306::size::DisplaySize128x64;
use ssd1306::Ssd1306Async;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

pub type SsdDisplay = Ssd1306Async<
    I2CInterface<I2c<'static, Async, Master>>,
    DisplaySize128x64,
    BufferedGraphicsModeAsync<DisplaySize128x64>,
>;

// static VIDEO_DATA: &[u8] = include_bytes!("../output.bin");

pub static TIM_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static REQUEST_NEXT_PAYLOAD_SIGNAL: Signal<ThreadModeRawMutex, u64> = Signal::new();
pub static PAYLOAD_SIGNAL: Signal<ThreadModeRawMutex, ([u8; PAYLOAD_SIZE], usize)> = Signal::new();

bind_interrupts!(struct Irqs {
    I2C1_EV => i2c::EventInterruptHandler<I2C1>;
    I2C1_ER => i2c::ErrorInterruptHandler<I2C1>;

    DMA1_STREAM6 => dma::InterruptHandler<DMA1_CH6>;
    DMA1_STREAM5 => dma::InterruptHandler<DMA1_CH5>;
    EXTI0 => exti::InterruptHandler<EXTI0>;

    OTG_FS => usb::InterruptHandler<USB_OTG_FS>;
});

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = Config::default();

    {
        use embassy_stm32::rcc::*;
        use embassy_stm32::time::Hertz;

        config.rcc.hse = Some(Hse {
            freq: Hertz(25_000_000),
            mode: HseMode::Oscillator,
        });
        config.rcc.pll_src = PllSource::HSE;

        config.rcc.pll = Some(Pll {
            prediv: PllPreDiv::DIV25,
            mul: PllMul::MUL192,
            divp: Some(PllPDiv::DIV2),
            divq: Some(PllQDiv::DIV4),
            divr: None,
        });

        config.rcc.sys = Sysclk::PLL1_P;
        config.rcc.ahb_pre = AHBPrescaler::DIV1;
        config.rcc.apb1_pre = APBPrescaler::DIV2;
        config.rcc.apb2_pre = APBPrescaler::DIV1;
    }

    let p = embassy_stm32::init(config);

    let led = Output::new(p.PC13, Level::High, Speed::Low);
    spawner.spawn(blink_led(led).unwrap());

    let mut i2c_config = i2c::Config::default();
    i2c_config.frequency = Hertz(400_000);
    let i2c = I2c::new(
        p.I2C1, p.PB8, p.PB9, p.DMA1_CH6, p.DMA1_CH5, Irqs, i2c_config,
    );

    let interface = ssd1306::I2CDisplayInterface::new(i2c);
    let display_base = Ssd1306Async::new(interface, DisplaySize128x64, Rotate0);

    let mut display = display_base.into_buffered_graphics_mode();
    display.init().await.unwrap();

    static EP_OUT_BUFFER: StaticCell<[u8; 256]> = StaticCell::new();
    let config = usb::Config::default();
    let driver = Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        EP_OUT_BUFFER.init([0u8; 256]),
        config,
    );

    let mut config = embassy_usb::Config::new(0x666, 0x666);
    config.manufacturer = Some("ex-exist limited");
    config.product = Some("Bad Apple player!");
    config.serial_number = Some("42");


    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    static STATE: StaticCell<State> = StaticCell::new();
    let state = STATE.init(State::new());
    let mut builder = Builder::new(
        driver,
        config,
        CONFIG_DESCRIPTOR.init([0; 256]),
        BOS_DESCRIPTOR.init([0; 256]),
        &mut [], // no msos descriptors
        CONTROL_BUF.init([0; 64]),
    );

    static CLASS: StaticCell<CdcAcmClass<Driver<USB_OTG_FS>>> = StaticCell::new();

    let class = CLASS.init(CdcAcmClass::new(&mut builder, state, 64));
    let mut usb = builder.build();
    let usb_fut = usb.run();

    let class_init_fut = async {
        class.wait_connection().await;
        info!("USB active!");
        let mut handshake_buf = [0u8; 9];

        if let Ok(9) = class.read_packet(&mut handshake_buf).await {
            if handshake_buf[0] == 0xAA {
                let video_length = u64::from_be_bytes(handshake_buf[1..].try_into().unwrap());
                spawner.spawn(update_display(display).unwrap());
                spawner.spawn(get_next_payload(class, video_length).unwrap());
            }
        }
    };

    join(usb_fut, class_init_fut).await;
}

#[embassy_executor::task]
async fn blink_led(mut led: Output<'static>) -> ! {
    loop {
        led.toggle();
        Timer::after_secs(1).await;
    }
}
