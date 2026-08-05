//! The 3×2 affine matrix PDF uses for every coordinate transform.
//!
//! PDF writes it as six numbers `[a b c d e f]`, standing for
//! `| a b 0 |` / `| c d 0 |` / `| e f 1 |`, with row vectors on the left — so
//! `p × M₁ × M₂` applies `M₁` first. `concat` keeps that order.

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Matrix {
        Matrix { a, b, c, d, e, f }
    }

    pub fn translate(tx: f32, ty: f32) -> Matrix {
        Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: tx, f: ty }
    }

    /// `self` applied first, then `other`.
    pub fn concat(self, other: Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    pub fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    /// How much this matrix scales a vertical unit — the factor that turns a
    /// font's nominal size into the size it is actually painted at.
    pub fn vertical_scale(self) -> f32 {
        (self.c * self.c + self.d * self.d).sqrt()
    }

    /// How much this matrix scales a horizontal unit.
    pub fn horizontal_scale(self) -> f32 {
        (self.a * self.a + self.b * self.b).sqrt()
    }
}

/// Build the transform from PDF user space to upright page space: origin at the
/// visible bottom-left, `y` increasing upward, `/Rotate` already applied.
pub(crate) fn page_transform(media_box: [f32; 4], rotate: i64) -> (Matrix, f32, f32) {
    let (x0, y0) = (media_box[0].min(media_box[2]), media_box[1].min(media_box[3]));
    let (x1, y1) = (media_box[0].max(media_box[2]), media_box[1].max(media_box[3]));
    let (w, h) = (x1 - x0, y1 - y0);
    match rotate.rem_euclid(360) {
        90 => (Matrix::new(0.0, -1.0, 1.0, 0.0, -y0, x1), h, w),
        180 => (Matrix::new(-1.0, 0.0, 0.0, -1.0, x1, y1), w, h),
        270 => (Matrix::new(0.0, 1.0, -1.0, 0.0, y1, -x0), h, w),
        _ => (Matrix::translate(-x0, -y0), w, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concat_applies_the_receiver_first() {
        let scale = Matrix::new(2.0, 0.0, 0.0, 2.0, 0.0, 0.0);
        let shift = Matrix::translate(10.0, 0.0);
        // Scale then shift: (1,1) → (2,2) → (12,2).
        assert_eq!(scale.concat(shift).apply(1.0, 1.0), (12.0, 2.0));
        // Shift then scale: (1,1) → (11,1) → (22,2).
        assert_eq!(shift.concat(scale).apply(1.0, 1.0), (22.0, 2.0));
    }

    #[test]
    fn rotation_moves_the_bottom_left_corner_where_the_viewer_sees_it() {
        let media = [0.0, 0.0, 600.0, 800.0];
        // 90° clockwise: the page's bottom-left corner is displayed top-left.
        let (m, w, h) = page_transform(media, 90);
        assert_eq!((w, h), (800.0, 600.0));
        assert_eq!(m.apply(0.0, 0.0), (0.0, 600.0));
        // 180°: bottom-left becomes top-right.
        let (m, _, _) = page_transform(media, 180);
        assert_eq!(m.apply(0.0, 0.0), (600.0, 800.0));
        // 270°: bottom-left becomes bottom-right.
        let (m, _, _) = page_transform(media, 270);
        assert_eq!(m.apply(0.0, 0.0), (800.0, 0.0));
    }

    #[test]
    fn an_offset_media_box_is_normalised_to_the_origin() {
        let (m, w, h) = page_transform([20.0, 30.0, 620.0, 830.0], 0);
        assert_eq!((w, h), (600.0, 800.0));
        assert_eq!(m.apply(20.0, 30.0), (0.0, 0.0));
    }
}
