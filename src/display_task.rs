use crate::{SsdDisplay, PAYLOAD_SIGNAL, REQUEST_NEXT_PAYLOAD_SIGNAL};
use defmt::info;
use embassy_stm32::i2c::{I2c, Master};
use embassy_stm32::mode::Async;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::geometry::Point;
use embedded_graphics::image::{Image, ImageRaw};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::Drawable;
use heatshrink::decoder::HeatshrinkDecoder;
use heatshrink::{Poll, SinkError};
use ssd1306::mode::BufferedGraphicsModeAsync;
use ssd1306::prelude::{DisplaySize128x64, I2CInterface};
use ssd1306::Ssd1306Async;

#[embassy_executor::task]
pub async fn update_display(
    mut display: Ssd1306Async<
        I2CInterface<I2c<'static, Async, Master>>,
        DisplaySize128x64,
        BufferedGraphicsModeAsync<DisplaySize128x64>,
    >,
) {
    loop {
        let mut decoder: HeatshrinkDecoder<10, 4, 128, 1024> = HeatshrinkDecoder::new();

        let mut output_buffer = [0u8; 1024];
        let mut output_buffer_size = 0;

        let mut flash_index: u64 = 0;
        REQUEST_NEXT_PAYLOAD_SIGNAL.signal(flash_index);

        let start_time = Instant::now();
        let mut frame_count = 0;
        let frame_duration = Duration::from_micros(33357);

        let (mut payload, mut length) = PAYLOAD_SIGNAL.wait().await;
        while length > 0 {
            info!("Started decoder sink! Flash index: {}", flash_index);
            match decoder.sink(&payload[..length]) {
                Ok(n) => {
                    flash_index += n as u64;
                }
                Err(SinkError::Full) => {}
                Err(SinkError::Misuse) => panic!("Misuse"),
            }

            loop {
                info!("Started inner loop!");
                let free_space = &mut output_buffer[output_buffer_size..];

                match decoder.poll(free_space) {
                    Ok(Poll::More(n)) => {
                        output_buffer_size += n;
                        if output_buffer_size == 1024 {
                            info!("Requesting more: {}", n);
                            // println!("More");

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
                        info!("Decoder is empty, break...");

                        output_buffer_size += n;
                        if output_buffer_size == 1024 {
                            // println!("Empty");
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
                        REQUEST_NEXT_PAYLOAD_SIGNAL.signal(flash_index);
                        (payload, length) = PAYLOAD_SIGNAL.wait().await;
                        break;
                    }
                    Err(e) => panic!("Err: {e:?}"),
                }
            }
        }
        Timer::after_millis(500).await;
    }
}

async fn draw_to_display(
    buf: &[u8],
    display: &mut SsdDisplay,
    start_time: Instant,
    frame_count: &mut u64,
    frame_duration: Duration,
) {
    let next_frame = start_time + (frame_duration * (*frame_count) as u32);
    *frame_count += 1;
    let raw = ImageRaw::<BinaryColor>::new(buf, 128);
    Image::new(&raw, Point::zero()).draw(display).unwrap();
    display.flush().await.unwrap();

    Timer::at(next_frame).await;
}
