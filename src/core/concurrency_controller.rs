use crate::models::step_option::InnerStepOption;
use std::cmp::min;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};

pub struct ConcurrencyController {
    semaphore: Arc<Semaphore>,
    total_permits: usize,
    step_option: Option<InnerStepOption>,
    fractional_accumulator: Mutex<f64>,
}

impl ConcurrencyController {
    pub fn new(total_permits: usize, step_option: Option<InnerStepOption>) -> Self {
        ConcurrencyController {
            semaphore: Arc::new(Semaphore::new(0)),
            total_permits,
            step_option,
            fractional_accumulator: Mutex::new(0.0),
        }
    }

    // 分发许可证
    pub async fn distribute_permits(&self) {
        match &self.step_option {
            Some(step_option) => {
                // 无论如何都先添加一个许可
                self.semaphore.add_permits(1);
                // 记录已经添加过的许可
                let mut permits_added = 1usize;
                {
                    let mut fractional_accumulator = self.fractional_accumulator.lock().await;
                    *fractional_accumulator += step_option.increase_step;
                    if *fractional_accumulator >= 1.0 {
                        let initial_permits_to_add = fractional_accumulator.floor() as usize;
                        self.semaphore.add_permits(initial_permits_to_add);
                        permits_added += initial_permits_to_add;
                        *fractional_accumulator -= initial_permits_to_add as f64;
                    }
                }
                while permits_added < self.total_permits {
                    tokio::time::sleep(Duration::from_secs(step_option.increase_interval)).await;
                    let mut fractional_accumulator = self.fractional_accumulator.lock().await;
                    *fractional_accumulator += step_option.increase_step;
                    let permits_to_add = min(
                        fractional_accumulator.floor() as usize,
                        self.total_permits - permits_added,
                    );
                    if permits_to_add > 0 {
                        self.semaphore.add_permits(permits_to_add);
                        permits_added += permits_to_add;
                        *fractional_accumulator -= permits_to_add as f64;
                    }
                }
            }
            None => {
                self.semaphore.add_permits(self.total_permits);
            }
        }
    }

    pub fn get_semaphore(&self) -> Arc<Semaphore> {
        self.semaphore.clone()
    }
}
