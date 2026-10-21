//! The maths runs in metres and seconds; people read feet, inches, and mph.

pub const FEET_PER_M: f64 = 1.0 / 0.3048;
pub const MPH_PER_MPS: f64 = 1.0 / 0.44704;

pub fn feet(m: f64) -> f64 {
    m * FEET_PER_M
}

pub fn metres(ft: f64) -> f64 {
    ft / FEET_PER_M
}

pub fn inches(m: f64) -> f64 {
    m * FEET_PER_M * 12.0
}

pub fn mph(mps: f64) -> f64 {
    mps * MPH_PER_MPS
}
