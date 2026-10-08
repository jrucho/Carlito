//! Geometry helpers. Drawing lives in surface.rs.

pub const SCREEN_W: usize = 1620;
pub const SCREEN_H: usize = 2160;

/// Grow-only pixel bounding box, used to build update/dissolve regions.
#[derive(Clone, Copy, Debug)]
pub struct BBox {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl BBox {
    pub fn empty() -> Self {
        Self {
            x0: i32::MAX,
            y0: i32::MAX,
            x1: i32::MIN,
            y1: i32::MIN,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.x0 > self.x1
    }
    pub fn add(&mut self, x: i32, y: i32, margin: i32) {
        self.x0 = self.x0.min(x - margin).max(0);
        self.y0 = self.y0.min(y - margin).max(0);
        self.x1 = self.x1.max(x + margin);
        self.y1 = self.y1.max(y + margin);
    }
    pub fn rect(&self) -> (i32, i32, i32, i32) {
        (
            self.x0,
            self.y0,
            self.x1 - self.x0 + 1,
            self.y1 - self.y0 + 1,
        )
    }
    pub fn rect_clamped(&self, w: usize, h: usize) -> (i32, i32, i32, i32) {
        if self.is_empty() || w == 0 || h == 0 {
            return (0, 0, 0, 0);
        }
        let x0 = self.x0.clamp(0, w as i32 - 1);
        let y0 = self.y0.clamp(0, h as i32 - 1);
        let x1 = self.x1.clamp(0, w as i32 - 1);
        let y1 = self.y1.clamp(0, h as i32 - 1);
        (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
    }
}
