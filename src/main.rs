#![no_std]
#![no_main]

use defmt::{info, println};
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
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::Builder;
use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::Point;
use embedded_graphics::Drawable;
use heatshrink::decoder::HeatshrinkDecoder;
use heatshrink::{Poll, SinkError};
use ssd1306::mode::{BufferedGraphicsModeAsync, DisplayConfigAsync};
use ssd1306::prelude::I2CInterface;
use ssd1306::rotation::DisplayRotation::Rotate0;
use ssd1306::size::DisplaySize128x64;
use ssd1306::Ssd1306Async;
use {defmt_rtt as _, panic_probe as _};

pub type SsdDisplay = Ssd1306Async<
    I2CInterface<I2c<'static, Async, Master>>,
    DisplaySize128x64,
    BufferedGraphicsModeAsync<DisplaySize128x64>,
>;

// static VIDEO_DATA: &[u8] = include_bytes!("../output.bin");

pub static TIM_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

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

    let mut ep_out_buffer = [0u8; 256];
    let config = usb::Config::default();
    let driver = Driver::new_fs(
        p.USB_OTG_FS,
        Irqs,
        p.PA12,
        p.PA11,
        &mut ep_out_buffer,
        config,
    );

    let mut config = embassy_usb::Config::new(0x666, 0x666);
    config.manufacturer = Some("ex-exist limited");
    config.product = Some("Bad Apple player!");
    config.serial_number = Some("42");

    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut control_buf = [0; 64];

    let mut state = State::new();
    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut [], // no msos descriptors
        &mut control_buf,
    );

    let mut class = CdcAcmClass::new(&mut builder, &mut state, 64);
    let mut usb = builder.build();
    let usb_fut = usb.run();

    let another_fut = async {
        loop {
            class.wait_connection().await;
            info!("USB active!");
            let mut handshake_buf = [0u8; 1];

            if let Ok(1) = class.read_packet(&mut handshake_buf).await {
                if handshake_buf[0] == 0xAA {
                    loop {
                        let mut decoder: HeatshrinkDecoder<10, 4, 128, 1024> =
                            HeatshrinkDecoder::new();

                        let mut input_buffer = [0u8; 1024];

                        let mut output_buffer = [0u8; 1024];
                        let mut output_buffer_size = 0;

                        let mut flash_index: u64 = 0;
                        let start_time = Instant::now();
                        let mut frame_count = 0;
                        let frame_duration = Duration::from_micros(31488);

                        get_next_data(&mut class, &mut input_buffer, flash_index).await;
                        while flash_index < 1482553 {
                            match decoder.sink(&input_buffer) {
                                Ok(n) => flash_index += n as u64,
                                Err(SinkError::Full) => {}
                                Err(SinkError::Misuse) => panic!("Misuse"),
                            }

                            loop {
                                let mut free_space = &mut output_buffer[output_buffer_size..];
                                if free_space.is_empty() {
                                    break;
                                }

                                match decoder.poll(&mut free_space) {
                                    Ok(Poll::More(n)) => {
                                        output_buffer_size += n;
                                        if output_buffer_size == 1024 {
                                            println!("More");

                                            draw_to_display(
                                                &output_buffer,
                                                &mut display,
                                                start_time,
                                                &mut frame_count,
                                                frame_duration,
                                            )
                                            .await;
                                            output_buffer_size = 0;
                                        }
                                    }
                                    Ok(Poll::Empty(n)) => {
                                        output_buffer_size += n;

                                        if output_buffer_size == 1024 {
                                            println!("Empty");
                                            draw_to_display(
                                                &output_buffer,
                                                &mut display,
                                                start_time,
                                                &mut frame_count,
                                                frame_duration,
                                            )
                                            .await;
                                            output_buffer_size = 0;
                                        };
                                        get_next_data(&mut class, &mut input_buffer, flash_index)
                                            .await;
                                        break;
                                    }
                                    Err(e) => panic!("Err: {e:?}"),
                                }
                            }
                        }
                    }
                }
            }

            Timer::after_millis(500).await;
        }
    };

    join(usb_fut, another_fut).await;
}

async fn get_next_data<'a>(
    class: &mut CdcAcmClass<'a, Driver<'a, USB_OTG_FS>>,
    input_buffer: &mut [u8; 1024],
    mut index: u64,
) {
    let mut write_buf = [0u8; 10];
    'outer: for i in 0..16 {
        write_buf[0..8].copy_from_slice(&index.to_be_bytes());
        write_buf[8..10].copy_from_slice(&64u16.to_be_bytes());
        if class.write_packet(&write_buf).await.is_ok() {
            let offset = i * 64;
            loop {
                match class
                    .read_packet(&mut input_buffer[offset..offset + 64])
                    .await
                {
                    Ok(bytes) => {
                        if bytes > 0 {
                            index += bytes as u64;
                            break;
                        }
                    }
                    Err(_) => {
                        break 'outer;
                    }
                }
            }
        }
    }
}

#[embassy_executor::task]
async fn blink_led(mut led: Output<'static>) -> ! {
    loop {
        led.toggle();
        Timer::after_secs(1).await;
    }
}

async fn draw_to_display(
    buf: &[u8],
    display: &mut SsdDisplay,
    start_time: Instant,
    frame_count: &mut u64,
    frame_duration: Duration,
) {
    info!("Draw!");
    let next_frame = start_time + (frame_duration * (*frame_count) as u32);
    *frame_count += 1;
    let raw = ImageRaw::<BinaryColor>::new(buf, 128);
    Image::new(&raw, Point::zero()).draw(display).unwrap();
    display.flush().await.unwrap();

    Timer::at(next_frame).await;
}
