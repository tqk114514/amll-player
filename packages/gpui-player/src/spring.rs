//! AMLL 弹簧动画系统的 Rust 移植。
//!
//! 逐行为对照 `@applemusic-like-lyrics/core` 的 `utils/spring.ts` 移植:
//! 解析闭式解 + 目标切换时的速度继承 + 单槽延迟队列。
//! 正确性由 `spring_fixture.json`(上游 TS 实现逐帧导出)做数值验收。
//!
//! 与上游的两个关键语义(移植时容易走样的地方):
//! 1. `reset_solver` 会把 `current_time` 归零——新解从 t=0 起算,
//!    延迟完全由队列倒计时承担,求解器自身的 delay 参数恒为 0;
//! 2. 只有"带延迟的目标/参数"才进队列(单槽,新目标覆盖旧的),
//!    无延迟调用立即重置求解器,不管当前是否已到达。

/// 完整的弹簧物理参数
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpringParams {
    pub mass: f64,
    pub damping: f64,
    pub stiffness: f64,
    pub soft: bool,
}

impl Default for SpringParams {
    fn default() -> Self {
        Self {
            mass: 1.0,
            damping: 10.0,
            stiffness: 100.0,
            soft: false,
        }
    }
}

impl SpringParams {
    /// 转为"全字段都提供"的增量补丁
    pub fn patch(self) -> SpringParamsPatch {
        SpringParamsPatch {
            stiffness: Some(self.stiffness),
            damping: Some(self.damping),
            mass: Some(self.mass),
            soft: Some(self.soft),
        }
    }
}

/// 参数增量:只更新提供的字段,其余保留原值(上游 `{...this.params, ...params}`)
#[derive(Debug, Clone, Copy, Default)]
pub struct SpringParamsPatch {
    pub stiffness: Option<f64>,
    pub damping: Option<f64>,
    pub mass: Option<f64>,
    pub soft: Option<bool>,
}

type SolverFn = Box<dyn Fn(f64) -> f64 + Send>;

const DERIVATIVE_STEP: f64 = 0.001;
/// 到达判定阈值:位移、速度、加速度均需小于该值
const ARRIVED_EPSILON: f64 = 0.01;
/// 无延迟时小于该值的目标变化直接忽略(并清空延迟队列)
const TARGET_EPSILON: f64 = 0.001;

/// 求解阻尼谐振子的解析闭式解(上游 solveSpring)。
/// `t < delay` 时返回 from,delay 之后按闭式解演化到 to。
fn solve_spring(
    from: f64,
    velocity: f64,
    to: f64,
    delay: f64,
    params: &SpringParams,
) -> SolverFn {
    let SpringParams {
        mass,
        damping,
        stiffness,
        soft,
    } = *params;
    let delta = to - from;

    if soft || 1.0 <= damping / (2.0 * (stiffness * mass).sqrt()) {
        // 过阻尼 / 临界阻尼
        let angular_frequency = -(stiffness / mass).sqrt();
        let leftover = -angular_frequency * delta - velocity;
        Box::new(move |t| {
            let t = t - delay;
            if t < 0.0 {
                return from;
            }
            to - (delta + t * leftover) * (t * angular_frequency).exp()
        })
    } else {
        // 欠阻尼
        let damping_frequency = (4.0 * mass * stiffness - damping * damping).sqrt();
        let leftover = (damping * delta - 2.0 * mass * velocity) / damping_frequency;
        let dfm = 0.5 * damping_frequency / mass;
        let dm = -(0.5 * damping) / mass;
        Box::new(move |t| {
            let t = t - delay;
            if t < 0.0 {
                return from;
            }
            to - ((t * dfm).cos() * delta + (t * dfm).sin() * leftover) * (t * dm).exp()
        })
    }
}

/// 中心差分求导(上游 derivative.ts,步长固定 1ms)
fn derivative(f: impl Fn(f64) -> f64) -> impl Fn(f64) -> f64 {
    move |x| (f(x + DERIVATIVE_STEP) - f(x - DERIVATIVE_STEP)) / (2.0 * DERIVATIVE_STEP)
}

/// 延迟队列项:单槽,新的调用会整体替换
#[derive(Debug, Clone, Copy)]
struct QueuedPosition {
    position: f64,
    time: f64,
}

#[derive(Debug, Clone, Copy)]
struct QueuedParams {
    patch: SpringParamsPatch,
    time: f64,
}

/// 弹簧状态机(上游 Spring 类的镜像)
pub struct Spring {
    current_position: f64,
    target_position: f64,
    current_time: f64,
    params: SpringParams,
    solver: SolverFn,
    queue_position: Option<QueuedPosition>,
    queue_params: Option<QueuedParams>,
    /// 仅供测试:记录每次求解器重置的 (时刻, 位置, 初速度),
    /// 用于与上游 fixture 的重置事件对比,验证速度继承语义
    #[cfg(test)]
    test_resets: Vec<(f64, f64, f64)>,
}

impl Spring {
    pub fn new(initial_position: f64) -> Self {
        Self {
            target_position: initial_position,
            current_position: initial_position,
            current_time: 0.0,
            params: SpringParams::default(),
            // 上游构造器直接放常量解
            solver: Box::new(move |_| initial_position),
            queue_position: None,
            queue_params: None,
            #[cfg(test)]
            test_resets: Vec::new(),
        }
    }

    pub fn current_position(&self) -> f64 {
        self.current_position
    }

    pub fn target_position(&self) -> f64 {
        self.target_position
    }

    fn current_velocity(&self) -> f64 {
        let solver = &self.solver;
        let v = derivative(move |t| solver(t));
        v(self.current_time)
    }

    fn current_acceleration(&self) -> f64 {
        let v = {
            let solver = &self.solver;
            derivative(move |t| solver(t))
        };
        let v2 = derivative(move |t| v(t));
        v2(self.current_time)
    }

    /// 当前值是否已稳定在目标上(上游 arrived())
    pub fn arrived(&self) -> bool {
        (self.target_position - self.current_position).abs() < ARRIVED_EPSILON
            && self.current_velocity().abs() < ARRIVED_EPSILON
            && self.current_acceleration().abs() < ARRIVED_EPSILON
            && self.queue_params.is_none()
            && self.queue_position.is_none()
    }

    /// 重置求解器:以当前解析速度为初速度,时间归零(上游 resetSolver)
    fn reset_solver(&mut self) {
        let cur_v = self.current_velocity();
        #[cfg(test)]
        self.test_resets
            .push((self.current_time, self.current_position, cur_v));
        self.current_time = 0.0;
        let position = self.current_position;
        let target = self.target_position;
        let params = self.params;
        self.solver = solve_spring(position, cur_v, target, 0.0, &params);
    }

    /// 立即钉死位置(上游 setPosition)
    pub fn set_position(&mut self, position: f64) {
        self.target_position = position;
        self.current_position = position;
        self.solver = Box::new(move |_| position);
    }

    /// 设置位置目标(上游 setTargetPosition)。
    /// `delay > 0` 进单槽延迟队列(新目标覆盖旧目标);否则立即重置求解器。
    pub fn set_target_position(&mut self, position: f64, delay: f64) {
        if delay <= 0.0 && (self.target_position - position).abs() < TARGET_EPSILON {
            self.queue_position = None;
            return;
        }
        if delay > 0.0 {
            self.queue_position = Some(QueuedPosition {
                position,
                time: delay,
            });
        } else {
            self.queue_position = None;
            self.target_position = position;
            self.reset_solver();
        }
    }

    /// 增量更新参数(上游 updateParams)。
    /// 立即生效时会清空位置延迟队列(与上游一致)。
    pub fn update_params(&mut self, patch: SpringParamsPatch, delay: f64) {
        if delay > 0.0 {
            self.queue_params = Some(QueuedParams { patch, time: delay });
        } else {
            self.queue_position = None;
            self.apply_params(patch);
            self.reset_solver();
        }
    }

    fn apply_params(&mut self, patch: SpringParamsPatch) {
        if let Some(stiffness) = patch.stiffness {
            self.params.stiffness = stiffness;
        }
        if let Some(damping) = patch.damping {
            self.params.damping = damping;
        }
        if let Some(mass) = patch.mass {
            self.params.mass = mass;
        }
        if let Some(soft) = patch.soft {
            self.params.soft = soft;
        }
    }

    /// 推进一帧;`delta` 为自上一帧的秒数。
    /// 顺序与上游一致:先求值当前位置,再处理两个延迟队列,最后做到达钉死。
    ///
    /// 上游的隐藏行为(经 fixture 逐帧验证,必须复刻):`queueParams`
    /// 触发后不会被清除,时间越减越负,之后每一帧都会重复触发
    /// updateParams + resetSolver——即延迟参数生效后弹簧改为
    /// "逐帧带速度重解",且 arrived() 从此恒为 false(不再钉死目标)。
    pub fn update(&mut self, delta: f64) {
        self.current_time += delta;
        self.current_position = (self.solver)(self.current_time);

        if let Some(queued) = self.queue_params {
            let time = queued.time - delta;
            let fired = time <= 0.0;
            self.queue_params = Some(QueuedParams {
                patch: queued.patch,
                time,
            });
            if fired {
                self.update_params(queued.patch, 0.0);
            }
        }
        if let Some(queued) = self.queue_position {
            let time = queued.time - delta;
            if time <= 0.0 {
                self.queue_position = None;
                self.set_target_position(queued.position, 0.0);
            } else {
                self.queue_position = Some(QueuedPosition {
                    position: queued.position,
                    time,
                });
            }
        }

        if self.arrived() {
            self.set_position(self.target_position);
        }
    }
}

/// 纵向滚动弹簧的动态刚度策略(上游 getPosYSpringPolicy)
pub mod pos_y_policy {
    use super::{SpringParams, SpringParamsPatch};

    const SLOW_STIFFNESS: f64 = 90.0;
    const SLOW_DAMPING: f64 = 15.0;
    const MEDIUM_STIFFNESS: f64 = 140.0;
    const MEDIUM_DAMPING: f64 = 22.0;
    const MIN_INTERVAL: f64 = 100.0;
    const MAX_INTERVAL: f64 = 800.0;
    const MIN_STIFFNESS: f64 = 170.0;
    const DAMPING_MULTIPLIER: f64 = 2.2;
    const INTERVAL_EXPONENT: f64 = 0.2;
    /// 上游所有 posY 弹簧的固定质量
    pub const MASS: f64 = 0.9;

    /// seek、间奏、歌曲首尾等场景的慢速参数
    pub fn slow() -> SpringParams {
        SpringParams {
            mass: MASS,
            stiffness: SLOW_STIFFNESS,
            damping: SLOW_DAMPING,
            soft: false,
        }
    }

    /// 歌曲播完时的一次性滚动
    pub fn medium() -> SpringParams {
        SpringParams {
            mass: MASS,
            stiffness: MEDIUM_STIFFNESS,
            damping: MEDIUM_DAMPING,
            soft: false,
        }
    }

    /// 普通播放:按当前行与上一行的时间间隔动态计算,
    /// 间隔越短刚度越大(170~220),阻尼取 2.2√k(略欠阻尼)
    pub fn by_interval_ms(interval_ms: f64) -> SpringParams {
        let clamped = interval_ms.clamp(MIN_INTERVAL, MAX_INTERVAL);
        let ratio = (1.0 - (clamped - MIN_INTERVAL) / (MAX_INTERVAL - MIN_INTERVAL))
            .powf(INTERVAL_EXPONENT);
        let stiffness = MIN_STIFFNESS + ratio * 50.0;
        SpringParams {
            mass: MASS,
            damping: stiffness.sqrt() * DAMPING_MULTIPLIER,
            stiffness,
            soft: false,
        }
    }

    /// 策略只需更新刚度与阻尼(上游不传 mass,保留原值)
    pub fn patch(params: SpringParams) -> SpringParamsPatch {
        SpringParamsPatch {
            stiffness: Some(params.stiffness),
            damping: Some(params.damping),
            mass: None,
            soft: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settles_to_target() {
        let mut spring = Spring::new(0.0);
        spring.set_target_position(100.0, 0.0);
        for _ in 0..600 {
            spring.update(1.0 / 60.0);
        }
        assert!(
            spring.arrived(),
            "应在 10 秒内到达,当前 {}",
            spring.current_position()
        );
        assert!((spring.current_position() - 100.0).abs() < ARRIVED_EPSILON);
    }

    #[test]
    fn holds_before_delay_then_settles() {
        let mut spring = Spring::new(0.0);
        spring.set_target_position(50.0, 0.5);
        for _ in 0..10 {
            spring.update(1.0 / 60.0);
        }
        assert!(spring.current_position().abs() < 1e-9, "延迟期间应保持在起点");
        assert!(!spring.arrived());
        for _ in 0..300 {
            spring.update(1.0 / 60.0);
        }
        assert!((spring.current_position() - 50.0).abs() < ARRIVED_EPSILON);
    }

    #[test]
    fn velocity_continuous_on_target_switch() {
        let mut spring = Spring::new(0.0);
        spring.set_target_position(100.0, 0.0);
        for _ in 0..30 {
            spring.update(1.0 / 60.0);
        }
        let v_before = spring.current_velocity();
        assert!(
            v_before.abs() > 0.01,
            "运动中切换目标才有意义,当前速度 {v_before}"
        );
        spring.set_target_position(200.0, 0.0);
        // 闭式解的初速度即传入的 v0,经一个极小 dt 后位置增量应吻合
        // (不能直接测 t=0 处的中心差分:上游求解器对 t<0 钳制到 from,
        //  与上游行为一致地会把差分测速污染成 v0/2)
        let dt = 0.001;
        let position_before = spring.current_position();
        spring.update(dt);
        let delta = spring.current_position() - position_before;
        assert!(
            (delta - v_before * dt).abs() < v_before.abs() * dt * 0.1,
            "切换后首个微步应按原速度继续: 期望 {delta}, 实际位移 {delta}"
        );
    }

    #[test]
    fn tiny_target_change_ignored_and_queue_cleared() {
        let mut spring = Spring::new(0.0);
        spring.set_target_position(10.0, 0.0);
        for _ in 0..300 {
            spring.update(1.0 / 60.0);
        }
        let arrived_at = spring.current_position();
        // 无延迟的微小变化:直接忽略并清空队列
        spring.set_target_position(10.0 + 1e-4, 0.0);
        assert_eq!(spring.target_position(), 10.0, "微小变化应被忽略");
        assert!(spring.arrived(), "无延迟时队列应被清空");
        assert!((spring.current_position() - arrived_at).abs() < 1e-12);
        // 带延迟的微小变化:先入队,倒计时到点触发时同样被忽略,自愈回 arrived
        spring.set_target_position(10.0 + 1e-4, 0.1);
        assert!(!spring.arrived(), "带延迟时应先进队列");
        for _ in 0..30 {
            spring.update(1.0 / 60.0);
        }
        assert!(spring.arrived(), "倒计时到点后微小目标被忽略,回到稳定");
        assert!((spring.current_position() - 10.0).abs() < ARRIVED_EPSILON);
    }

    #[test]
    fn frame_delta_clamped_by_caller_contract() {
        // 上游不做帧钳制,由播放器把 delta 限制在 100ms 内;
        // 这里验证大 delta 下闭式解不发散
        let mut spring = Spring::new(0.0);
        spring.set_target_position(100.0, 0.0);
        spring.update(0.1);
        let position = spring.current_position();
        assert!(
            position > 0.0 && position < 100.0,
            "100ms 内不应到达目标,当前 {position}"
        );
        assert!(
            (position - 34.1).abs() < 0.5,
            "闭式解应在预期范围内,当前 {position}"
        );
    }

    #[test]
    fn pos_y_policy_by_interval() {
        let fast = pos_y_policy::by_interval_ms(100.0);
        assert!((fast.stiffness - 220.0).abs() < 1e-9);
        assert!((fast.damping - 220.0_f64.sqrt() * 2.2).abs() < 1e-9);
        let slow = pos_y_policy::by_interval_ms(800.0);
        assert!((slow.stiffness - 170.0).abs() < 1e-9);
        let mid = pos_y_policy::by_interval_ms(450.0);
        assert!(mid.stiffness > 170.0 && mid.stiffness < 220.0);
        assert!((mid.mass - 0.9).abs() < 1e-9);
    }

    /// 与上游 TS 实现的逐帧数值验收:fixture 由
    /// scripts/gen-spring-fixture.mjs 从 amll-core.mjs 提取的真实
    /// Spring 类跑出,操作序列在两端完全一致(时长单位为 ms)。
    #[test]
    fn matches_upstream_fixture() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("testdata")
            .join("spring-fixture.json");
        let data = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 fixture 失败({}): {e}", path.display()));
        let parsed: serde_json::Value =
            serde_json::from_str(&data).expect("fixture 应为合法 JSON");

        let scenarios = parsed["results"]
            .as_array()
            .expect("fixture 应包含 results 数组");
        assert!(!scenarios.is_empty(), "fixture 不应为空");

        let mut max_diff = 0.0_f64;
        let mut total_frames = 0_usize;
        for scenario in scenarios {
            let name = scenario["name"].as_str().unwrap_or("<未命名>");
            let mut spring = Spring::new(0.0);
            let frames = scenario["frames"].as_array().expect("场景应含帧序列");
            let mut frame_index = 0_usize;
            for op in scenario["ops"].as_array().expect("场景应含操作序列") {
                match op["t"].as_str().unwrap_or_default() {
                    "target" => {
                        let position = op["position"].as_f64().expect("target 应含 position");
                        let delay = op["delay"].as_f64().unwrap_or(0.0) / 1000.0;
                        spring.set_target_position(position, delay);
                    }
                    "params" => {
                        let patch = SpringParamsPatch {
                            stiffness: op["stiffness"].as_f64(),
                            damping: op["damping"].as_f64(),
                            mass: op["mass"].as_f64(),
                            soft: op["soft"].as_bool(),
                        };
                        let delay = op["delay"].as_f64().unwrap_or(0.0) / 1000.0;
                        spring.update_params(patch, delay);
                    }
                    "update" => {
                        let dt = op["dt"].as_f64().expect("update 应含 dt") / 1000.0;
                        spring.update(dt);
                        let expected = frames[frame_index]
                            .as_f64()
                            .unwrap_or_else(|| panic!("{name} 第 {frame_index} 帧缺失"));
                        let diff = (spring.current_position() - expected).abs();
                        max_diff = max_diff.max(diff);
                        total_frames += 1;
                        frame_index += 1;
                        assert!(
                            diff < 1e-6,
                            "{name} 第 {frame_index} 帧偏差 {diff}: 实际 {} 期望 {expected}",
                            spring.current_position()
                        );
                    }
                    other => panic!("{name} 含未知操作: {other}"),
                }
            }
            assert_eq!(frame_index, frames.len(), "{name} 帧数不匹配");
            // 重置事件对比:时间/位置精确吻合,初速度(中心差分导数,数值放大)放宽到 1e-4
            let resets: Vec<&serde_json::Value> = scenario["resets"]
                .as_array()
                .map(|array| array.iter().collect())
                .unwrap_or_default();
            assert_eq!(
                spring.test_resets.len(),
                resets.len(),
                "{name} 重置次数不匹配: 实际 {} 期望 {}",
                spring.test_resets.len(),
                resets.len()
            );
            for (index, (actual, expected)) in spring.test_resets.iter().zip(resets).enumerate() {
                let t = expected["t"].as_f64().unwrap_or_default();
                let from = expected["from"].as_f64().unwrap_or_default();
                let v0 = expected["v0"].as_f64().unwrap_or_default();
                assert!(
                    (actual.0 - t).abs() < 1e-9 && (actual.1 - from).abs() < 1e-6,
                    "{name} 第 {index} 次重置状态不匹配: 实际 ({}, {}) 期望 ({t}, {from})",
                    actual.0,
                    actual.1
                );
                assert!(
                    (actual.2 - v0).abs() < 1e-4,
                    "{name} 第 {index} 次重置初速度不匹配: 实际 {} 期望 {v0}",
                    actual.2
                );
            }
        }
        println!("fixture 验收: {total_frames} 帧,最大偏差 {max_diff:e}");
    }


}
