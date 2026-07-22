use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Window;

const BACKGROUND: u32 = 0x00ff_ffff;
const FOREGROUND: u32 = 0x001c_1c20;
const TRACK: u32 = 0x00e6_e6ea;
const ACCENT: u32 = 0x001a_7fdb;

#[derive(Clone)]
pub struct StartupRenderer {
    running: Arc<AtomicBool>,
}

impl StartupRenderer {
    pub fn start(window: Window) -> Result<Self, Box<dyn std::error::Error>> {
        let size = window.inner_size()?;
        let width = NonZeroU32::new(size.width.max(1)).unwrap();
        let height = NonZeroU32::new(size.height.max(1)).unwrap();
        let context = Context::new(window.clone())?;
        let mut surface = Surface::new(&context, window)?;
        surface.resize(width, height)?;
        render_surface(&mut surface, 0.0)?;

        let running = Arc::new(AtomicBool::new(true));
        let thread_running = running.clone();
        std::thread::spawn(move || {
            let _context = context;
            let started = Instant::now();
            while thread_running.load(Ordering::Acquire) {
                if render_surface(&mut surface, started.elapsed().as_secs_f32()).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(16));
            }
        });
        Ok(Self { running })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }
}

fn render_surface(
    surface: &mut Surface<Window, Window>,
    elapsed_seconds: f32,
) -> Result<(), softbuffer::SoftBufferError> {
    let mut buffer = surface.buffer_mut()?;
    let width = buffer.width().get() as usize;
    let height = buffer.height().get() as usize;
    draw_frame(
        &mut buffer,
        width,
        height,
        startup_progress(elapsed_seconds),
    );
    buffer.present()
}

fn startup_progress(elapsed_seconds: f32) -> f32 {
    let t = (elapsed_seconds / 8.0).clamp(0.0, 1.0);
    if t < 0.4 {
        interpolate(0.05, 0.55, t / 0.4)
    } else if t < 0.7 {
        interpolate(0.55, 0.78, (t - 0.4) / 0.3)
    } else {
        interpolate(0.78, 0.9, (t - 0.7) / 0.3)
    }
}

fn interpolate(from: f32, to: f32, t: f32) -> f32 {
    let eased = 1.0 - (1.0 - t).powi(3);
    from + (to - from) * eased
}

fn draw_frame(buffer: &mut [u32], width: usize, height: usize, progress: f32) {
    buffer.fill(BACKGROUND);
    let scale = (width / 400).clamp(3, 6);
    let label_width = text_width("LogCrate", scale);
    let label_x = width.saturating_sub(label_width) / 2;
    let center_y = height / 2;
    let label_y = center_y.saturating_sub(40 * scale / 3);
    draw_text(buffer, width, height, label_x, label_y, scale, "LogCrate");

    let track_width = (width * 220 / 1200)
        .clamp(160, 440)
        .min(width.saturating_sub(32));
    let track_height = (width * 4 / 1200).clamp(4, 8);
    let track_x = width.saturating_sub(track_width) / 2;
    let track_y = center_y + 20 * scale / 3;
    fill_rect(
        buffer,
        width,
        height,
        track_x,
        track_y,
        track_width,
        track_height,
        TRACK,
    );
    fill_rect(
        buffer,
        width,
        height,
        track_x,
        track_y,
        ((track_width as f32 * progress) as usize).max(1),
        track_height,
        ACCENT,
    );
}

fn draw_text(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    mut x: usize,
    y: usize,
    scale: usize,
    text: &str,
) {
    for character in text.chars() {
        let glyph = glyph(character);
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    fill_rect(
                        buffer,
                        width,
                        height,
                        x + column * scale,
                        y + row * scale,
                        scale,
                        scale,
                        FOREGROUND,
                    );
                }
            }
        }
        x += 6 * scale;
    }
}

fn text_width(text: &str, scale: usize) -> usize {
    text.chars().count() * 6 * scale - scale
}

#[allow(clippy::too_many_arguments)]
fn fill_rect(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    rect_width: usize,
    rect_height: usize,
    color: u32,
) {
    let right = (x + rect_width).min(width);
    let bottom = (y + rect_height).min(height);
    for row in y.min(height)..bottom {
        buffer[row * width + x.min(width)..row * width + right].fill(color);
    }
}

fn glyph(character: char) -> [u8; 7] {
    match character {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'G' => [
            0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'a' => [
            0b00000, 0b01110, 0b00001, 0b01111, 0b10001, 0b10011, 0b01101,
        ],
        'e' => [
            0b00000, 0b01110, 0b10001, 0b11111, 0b10000, 0b10000, 0b01111,
        ],
        'g' => [
            0b00000, 0b01110, 0b10001, 0b01111, 0b00001, 0b10001, 0b01110,
        ],
        'o' => [
            0b00000, 0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'r' => [
            0b00000, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000, 0b10000,
        ],
        't' => [
            0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00100, 0b00011,
        ],
        _ => [0; 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_progress_advances_without_reaching_completion() {
        assert!(startup_progress(0.0) > 0.0);
        assert!(startup_progress(2.0) < startup_progress(5.0));
        assert_eq!(startup_progress(20.0), 0.9);
    }

    #[test]
    fn frame_draws_brand_and_progress_over_white_background() {
        let mut pixels = vec![0; 320 * 200];
        draw_frame(&mut pixels, 320, 200, 0.5);
        assert!(pixels.contains(&FOREGROUND));
        assert!(pixels.contains(&TRACK));
        assert!(pixels.contains(&ACCENT));
        assert!(pixels.contains(&BACKGROUND));
    }
}
