pub(crate) struct ExponentialMovingAverage {
    /// 指数滑动平均
    alpha: f64,
    current_ema: f64,
    initialized: bool,
}

impl ExponentialMovingAverage {
    pub(crate) fn new(alpha: f64) -> Self {
        Self {
            alpha,
            current_ema: 0.0,
            initialized: false,
        }
    }

    pub(crate) fn add(&mut self, value: f64) -> f64 {
        if !self.initialized {
            self.current_ema = value;
            self.initialized = true;
        } else {
            self.current_ema = self.alpha * value + (1.0f64 - self.alpha) * self.current_ema;
        }
        self.current_ema
    }
}
