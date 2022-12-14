use macroquad::prelude::*;
use ordered_float::{Float, NotNan};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::Poll,
};

pub trait NotNanExt: Sized {
    fn not_nan(self) -> NotNan<Self>;
}

impl<T: Sized + Float> NotNanExt for T {
    fn not_nan(self) -> NotNan<Self> {
        NotNan::new(self).unwrap()
    }
}

pub fn draw_text_aligned(font: Font, text: &str, x: f32, y: f32, anchor: (f32, f32), scale: f32, color: Color) -> Rect {
    use macroquad::prelude::*;
    let size = (screen_width() / 23. * scale) as u16;
    let scale = 0.08 * scale / size as f32;
    let dim = measure_text(text, Some(font), size, scale);
    let rect = Rect::new(x - dim.width * anchor.0, y - dim.offset_y * anchor.1, dim.width, dim.offset_y);
    draw_text_ex(
        text,
        rect.x,
        rect.y + dim.offset_y,
        TextParams {
            font,
            font_size: size,
            font_scale: scale,
            color,
            ..Default::default()
        },
    );
    rect
}

pub const PARALLELOGRAM_SLOPE: f32 = 0.13 / (7. / 13.);

pub fn draw_parallelogram(rect: Rect, texture: Option<(Texture2D, Rect)>, color: Color) {
    draw_parallelogram_ex(rect, texture, color, color);
}

pub fn draw_parallelogram_ex(rect: Rect, texture: Option<(Texture2D, Rect)>, top: Color, bottom: Color) {
    let l = rect.h * PARALLELOGRAM_SLOPE;
    let gl = unsafe { get_internal_gl() }.quad_gl;
    let p = if let Some((tex, tex_rect)) = texture {
        let lt = tex_rect.h * PARALLELOGRAM_SLOPE;
        gl.texture(Some(tex));
        [
            Vertex::new(rect.x + l, rect.y, 0., tex_rect.x + lt, tex_rect.y, top),
            Vertex::new(rect.right(), rect.y, 0., tex_rect.right(), tex_rect.y, top),
            Vertex::new(rect.x, rect.bottom(), 0., tex_rect.x, tex_rect.bottom(), bottom),
            Vertex::new(rect.right() - l, rect.bottom(), 0., tex_rect.right() - lt, tex_rect.bottom(), bottom),
        ]
    } else {
        gl.texture(None);
        [
            Vertex::new(rect.x + l, rect.y, 0., 0., 0., top),
            Vertex::new(rect.right(), rect.y, 0., 0., 0., top),
            Vertex::new(rect.x, rect.bottom(), 0., 0., 0., bottom),
            Vertex::new(rect.right() - l, rect.bottom(), 0., 0., 0., bottom),
        ]
    };
    gl.draw_mode(DrawMode::Triangles);
    gl.geometry(&p, &[0, 2, 3, 0, 1, 3]);
}

pub fn thread_as_future<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> impl Future<Output = R> {
    struct DummyFuture<R>(Arc<Mutex<Option<R>>>);
    impl<R> Future for DummyFuture<R> {
        type Output = R;

        fn poll(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> Poll<Self::Output> {
            match self.0.lock().unwrap().take() {
                Some(res) => Poll::Ready(res),
                None => Poll::Pending,
            }
        }
    }
    let arc = Arc::new(Mutex::new(None));
    std::thread::spawn({
        let arc = Arc::clone(&arc);
        move || {
            let res = f();
            *arc.lock().unwrap() = Some(res);
        }
    });
    DummyFuture(arc)
}
