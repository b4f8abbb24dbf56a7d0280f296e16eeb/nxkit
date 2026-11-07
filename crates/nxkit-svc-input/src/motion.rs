//! Controller motion for the six-axis sensor, a port of nxnative `motion_fusion.h` (MIT).
//!
//! Input is what `GCMotion` reports in the controller's own frame: acceleration in G (gravity direction, so
//! (0, 0, -1) lying face up) and rotation rate in rad/s. Output is the `SixAxisSensorState` data: acceleration in G,
//! angular velocity in revolutions per second, angle in revolutions and the three axes of the controller in the
//! world. The Switch frame is x right, y forward, z up out of the face.
//!
//! The gyro is integrated. Tilt drift is pulled back to the measured gravity (Mahony) while the acceleration is near
//! 1 G. Yaw drifts freely, as on the console without a recenter.

use std::f32::consts::PI;

/// A signed permutation from the axes Apple reports to the axes of the Switch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MotionMap {
    src: [usize; 3],
    sign: [f32; 3],
    acc_sign: f32,
    det: f32,
}

impl Default for MotionMap {
    fn default() -> Self {
        MotionMap { src: [0, 1, 2], sign: [1.0; 3], acc_sign: 1.0, det: 1.0 }
    }
}

impl MotionMap {
    /// Parses `"x,y,z"` with an optional sign on each entry, for example `"x,-z,y"`. `None` for a malformed string.
    pub(crate) fn parse(s: &str) -> Option<MotionMap> {
        let mut m = MotionMap::default();
        let mut used = 0u32;
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != 3 {
            return None;
        }
        for (i, part) in parts.iter().enumerate() {
            let t = part.trim();
            let (sg, rest) = match t.as_bytes().first() {
                Some(b'-') => (-1.0, &t[1..]),
                Some(b'+') => (1.0, &t[1..]),
                _ => (1.0, t),
            };
            let a = match rest.trim() {
                "x" | "X" => 0,
                "y" | "Y" => 1,
                "z" | "Z" => 2,
                _ => return None,
            };
            if used >> a & 1 == 1 {
                return None;
            }
            used |= 1 << a;
            m.src[i] = a;
            m.sign[i] = sg;
        }
        let inv = (m.src[0] > m.src[1]) as u32 + (m.src[0] > m.src[2]) as u32 + (m.src[1] > m.src[2]) as u32;
        m.det = m.sign[0] * m.sign[1] * m.sign[2] * if inv & 1 == 1 { -1.0 } else { 1.0 };
        Some(m)
    }

    /// The same map with the acceleration sign set. -1 is for a source that reports the specific force.
    pub(crate) fn with_acc_sign(mut self, s: f32) -> MotionMap {
        self.acc_sign = s;
        self
    }

    fn apply(&self, v: [f32; 3], scale: f32) -> [f32; 3] {
        [0, 1, 2].map(|i| self.sign[i] * v[self.src[i]] * scale)
    }
}

/// The fusion state of one controller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MotionState {
    /// Controller to world. The columns are the controller axes in the world.
    r: [[f32; 3]; 3],
    angle: [f32; 3],
    acc: [f32; 3],
    gyro: [f32; 3],
    primed: bool,
}

impl Default for MotionState {
    fn default() -> Self {
        MotionState {
            r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            angle: [0.0; 3],
            acc: [0.0, 0.0, -1.0],
            gyro: [0.0; 3],
            primed: false,
        }
    }
}

/// The fields of a `SixAxisSensorState` that motion fills.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SixAxisData {
    /// Acceleration in G.
    pub(crate) acc: [f32; 3],
    /// Angular velocity in revolutions per second.
    pub(crate) gyro: [f32; 3],
    /// Angle in revolutions.
    pub(crate) angle: [f32; 3],
    /// The three axes of the controller in the world, row by row.
    pub(crate) dir: [f32; 9],
}

impl SixAxisData {
    /// A controller lying flat and still.
    pub(crate) fn rest() -> SixAxisData {
        MotionState::default().data()
    }
}

fn mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut t = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            t[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    t
}

/// Gram-Schmidt on the columns.
fn orthonormalize(r: &mut [[f32; 3]; 3]) {
    let mut c = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            c[j][i] = r[i][j];
        }
    }
    for k in 0..3 {
        for p in 0..k {
            let d = c[k][0] * c[p][0] + c[k][1] * c[p][1] + c[k][2] * c[p][2];
            for i in 0..3 {
                c[k][i] -= d * c[p][i];
            }
        }
        let mut n = (c[k][0] * c[k][0] + c[k][1] * c[k][1] + c[k][2] * c[k][2]).sqrt();
        if n < 1e-6 {
            n = 1.0;
        }
        for i in 0..3 {
            c[k][i] /= n;
        }
    }
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = c[j][i];
        }
    }
}

fn norm(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

impl MotionState {
    /// Starts level with the measured gravity, facing forward.
    fn prime(&mut self, a: [f32; 3]) {
        let n = norm(a);
        if n > 0.5 && n < 1.5 {
            let z = [-a[0] / n, -a[1] / n, -a[2] / n];
            let mut y0 = [0.0, 1.0, 0.0];
            if z[1].abs() > 0.99 {
                y0 = [1.0, 0.0, 0.0];
            }
            let d = y0[0] * z[0] + y0[1] * z[1] + y0[2] * z[2];
            let mut y = [y0[0] - d * z[0], y0[1] - d * z[1], y0[2] - d * z[2]];
            let yn = norm(y);
            if yn > 1e-3 {
                for v in &mut y {
                    *v /= yn;
                }
                let x = cross(y, z);
                for j in 0..3 {
                    self.r[0][j] = x[j];
                    self.r[1][j] = y[j];
                    self.r[2][j] = z[j];
                }
            }
        }
        self.primed = true;
    }

    /// One sample. `acc_raw` in G and `rot_raw` in rad/s as Apple reports them, `dt` in seconds since the previous
    /// sample, `k` the tilt correction gain. A sample with a non-finite value is dropped.
    pub(crate) fn update(&mut self, map: &MotionMap, acc_raw: [f32; 3], rot_raw: [f32; 3], dt: f32, k: f32) {
        if acc_raw.iter().chain(rot_raw.iter()).any(|v| !v.is_finite()) {
            return;
        }
        let dt = if dt.is_finite() && dt >= 0.0 { dt } else { 0.0 };
        let k = if k.is_finite() && k >= 0.0 { k } else { 0.0 };
        let a = map.apply(acc_raw, map.acc_sign);
        let mut w = map.apply(rot_raw, map.det);
        if !self.primed || dt <= 0.0 || dt > 0.25 {
            if !self.primed {
                self.prime(a);
            }
        } else {
            let n = norm(a);
            if k > 0.0 && n > 0.8 && n < 1.2 {
                let gm = [a[0] / n, a[1] / n, a[2] / n];
                let gp = [-self.r[2][0], -self.r[2][1], -self.r[2][2]];
                let e = cross(gm, gp);
                for i in 0..3 {
                    w[i] += k * e[i];
                }
            }
            let th = norm(w) * dt;
            if th.is_finite() && th > 1e-9 {
                let u = [w[0] * dt / th, w[1] * dt / th, w[2] * dt / th];
                let (sn, c) = th.sin_cos();
                let v = 1.0 - c;
                let e = [
                    [c + u[0] * u[0] * v, u[0] * u[1] * v - u[2] * sn, u[0] * u[2] * v + u[1] * sn],
                    [u[1] * u[0] * v + u[2] * sn, c + u[1] * u[1] * v, u[1] * u[2] * v - u[0] * sn],
                    [u[2] * u[0] * v - u[1] * sn, u[2] * u[1] * v + u[0] * sn, c + u[2] * u[2] * v],
                ];
                self.r = mul(&self.r, &e);
                orthonormalize(&mut self.r);
            }
        }
        let wraw = map.apply(rot_raw, map.det / (2.0 * PI));
        for i in 0..3 {
            self.gyro[i] = wraw[i];
            self.acc[i] = a[i];
            if dt > 0.0 && dt <= 0.25 {
                let angle = self.angle[i] + wraw[i] * dt;
                if angle.is_finite() {
                    self.angle[i] = angle;
                }
            }
        }
    }

    /// The `SixAxisSensorState` data of the state.
    pub(crate) fn data(&self) -> SixAxisData {
        let mut dir = [0.0; 9];
        for i in 0..3 {
            for j in 0..3 {
                dir[3 * i + j] = self.r[j][i];
            }
        }
        SixAxisData { acc: self.acc, gyro: self.gyro, angle: self.angle, dir }
    }

    /// The sum of the absolute angular velocities, in revolutions per second.
    pub(crate) fn spin(&self) -> f32 {
        self.gyro.iter().map(|g| g.abs()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn the_default_map_is_the_identity() {
        assert_eq!(MotionMap::parse("x,y,z"), Some(MotionMap::default()));
    }

    #[test]
    fn a_swap_has_determinant_minus_one() {
        let m = MotionMap::parse("y,x,z");
        assert_eq!(m.map(|m| m.det), Some(-1.0));
        assert_eq!(MotionMap::parse("x,-z,y").map(|m| m.det), Some(1.0));
    }

    #[test]
    fn malformed_maps_are_refused() {
        assert_eq!(MotionMap::parse("x,x,z"), None);
        assert_eq!(MotionMap::parse("x,y"), None);
        assert_eq!(MotionMap::parse("x,y,w"), None);
        assert_eq!(MotionMap::parse(""), None);
    }

    #[test]
    fn rest_is_gravity_down_and_the_identity() {
        let d = SixAxisData::rest();
        assert_eq!(d.acc, [0.0, 0.0, -1.0]);
        assert_eq!(d.dir, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        assert_eq!(d.gyro, [0.0; 3]);
    }

    #[test]
    fn a_flat_still_controller_stays_level() {
        let mut s = MotionState::default();
        let map = MotionMap::default();
        for _ in 0..10 {
            s.update(&map, [0.0, 0.0, -1.0], [0.0; 3], 0.01, 0.5);
        }
        let d = s.data();
        assert!(close(d.dir[8], 1.0));
        assert!(close(d.acc[2], -1.0));
        assert!(s.spin() < 1e-6);
    }

    #[test]
    fn a_turn_around_z_integrates_to_the_angle() {
        let mut s = MotionState::default();
        let map = MotionMap::default();
        s.update(&map, [0.0, 0.0, -1.0], [0.0; 3], 0.0, 0.5);
        for _ in 0..100 {
            s.update(&map, [0.0, 0.0, -1.0], [0.0, 0.0, 2.0 * PI], 0.01, 0.5);
        }
        let d = s.data();
        assert!(close(d.angle[2], 1.0));
        assert!(close(d.gyro[2], 1.0));
    }

    #[test]
    fn a_non_finite_sample_is_dropped() {
        let mut s = MotionState::default();
        s.update(&MotionMap::default(), [f32::NAN, 0.0, -1.0], [0.0; 3], 0.01, 0.5);
        assert_eq!(s, MotionState::default());
    }
}
