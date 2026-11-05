//! Port of `packages/timer/tests/index.spec.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cordis_core::{Context, Fiber, Inject, PluginDef};
use cordis_timer::{Disposed, Timer, TimerContext, TimerExt, TimerPlugin};
use serde_json::Value;

async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

const SECOND: Duration = Duration::from_millis(1000);

/// A root with the timer plugin and a consumer fiber declaring `timer`.
async fn with_context() -> (Context, Fiber) {
    let root = Context::new();
    root.plugin::<TimerPlugin>(()).unwrap().settled().await.unwrap();
    let fiber = root
        .plugin_def(&PluginDef::new(|_, _| async { Ok(()) }).inject(Inject::of::<(Timer,)>()), Value::Null)
        .unwrap();
    fiber.settled().await.unwrap();
    (root, fiber)
}

fn counter() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + Clone + 'static) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    (calls, move || {
        c.fetch_add(1, Ordering::SeqCst);
    })
}

fn n(calls: &Arc<AtomicUsize>) -> usize {
    calls.load(Ordering::SeqCst)
}

#[tokio::test(start_paused = true)]
async fn timeout_basic_support() {
    let (_, fiber) = with_context().await;
    let ctx = fiber.context();
    let (calls, callback) = counter();
    ctx.timer().unwrap().timeout(SECOND, callback).unwrap();
    assert_eq!(fiber.effect_count(), 1);
    assert_eq!(n(&calls), 0);
    advance(1000).await;
    assert_eq!(n(&calls), 1);
    // a spent timer is no longer an effect of the fiber
    assert_eq!(fiber.effect_count(), 0);
    advance(1000).await;
    assert_eq!(n(&calls), 1);
}

#[tokio::test(start_paused = true)]
async fn timeout_dispose() {
    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let handle = fiber.context().timer().unwrap().timeout(SECOND, callback).unwrap();
    handle.dispose().await.unwrap();
    advance(2000).await;
    assert_eq!(n(&calls), 0);
}

#[tokio::test(start_paused = true)]
async fn sleep_resolves_or_fails_with_the_context() {
    let (_, fiber) = with_context().await;
    let timer = fiber.context().timer().unwrap();
    let sleeping = tokio::spawn(timer.sleep(SECOND));
    advance(500).await;
    assert!(!sleeping.is_finished());
    advance(500).await;
    sleeping.await.unwrap().unwrap();

    let sleeping = tokio::spawn(timer.sleep(SECOND));
    advance(100).await;
    fiber.dispose().await.unwrap();
    let error = sleeping.await.unwrap().unwrap_err();
    assert!(error.is::<Disposed>());
}

#[tokio::test(start_paused = true)]
async fn interval_basic_support() {
    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let handle = fiber.context().timer().unwrap().interval(SECOND, callback).unwrap();
    assert_eq!(n(&calls), 0);
    advance(1000).await;
    assert_eq!(n(&calls), 1);
    advance(1000).await;
    assert_eq!(n(&calls), 2);
    handle.dispose().await.unwrap();
    advance(2000).await;
    assert_eq!(n(&calls), 2);
}

#[tokio::test(start_paused = true)]
async fn ticks_until_closed() {
    let (_, fiber) = with_context().await;
    let mut ticks = fiber.context().timer().unwrap().ticks(SECOND).unwrap();
    let started = tokio::time::Instant::now();
    ticks.tick().await.unwrap();
    ticks.tick().await.unwrap();
    assert_eq!(started.elapsed(), 2 * SECOND);
    assert_eq!(fiber.effect_count(), 1);
    ticks.close();
    assert_eq!(fiber.effect_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn ticks_fail_when_the_context_is_disposed() {
    let (_, fiber) = with_context().await;
    let mut ticks = fiber.context().timer().unwrap().ticks(SECOND).unwrap();
    let reading = tokio::spawn(async move { ticks.tick().await });
    advance(500).await;
    fiber.dispose().await.unwrap();
    assert_eq!(reading.await.unwrap(), Err(Disposed));
}

#[tokio::test(start_paused = true)]
async fn interval_async_iterator_break_return() {
    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let mut ticks = fiber.context().timer().unwrap().ticks(SECOND).unwrap();
    let iterate = tokio::spawn(async move {
        let mut i = 0;
        loop {
            ticks.tick().await?;
            i += 1;
            if i > 2 {
                break;
            }
            callback();
        }
        Ok::<_, Disposed>(())
    });
    assert_eq!(n(&calls), 0);
    advance(1000).await;
    assert_eq!(n(&calls), 1);
    advance(1000).await;
    assert_eq!(n(&calls), 2);
    advance(1000).await;
    assert_eq!(n(&calls), 2);
    assert_eq!(iterate.await.unwrap(), Ok(()));
    // leaving the loop drops the ticks, which disposes the interval
    assert_eq!(fiber.effect_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn interval_async_iterator_break_throw() {
    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let mut ticks = fiber.context().timer().unwrap().ticks(SECOND).unwrap();
    let iterate = tokio::spawn(async move {
        let mut i = 0;
        loop {
            ticks.tick().await?;
            i += 1;
            if i > 2 {
                return Err::<(), _>(cordis_core::Error::msg("test"));
            }
            callback();
        }
    });
    advance(1000).await;
    assert_eq!(n(&calls), 1);
    advance(1000).await;
    assert_eq!(n(&calls), 2);
    advance(1000).await;
    assert_eq!(n(&calls), 2);
    assert_eq!(iterate.await.unwrap().unwrap_err().to_string(), "test");
    assert_eq!(fiber.effect_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn throttle_basic_support() {
    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let throttled = fiber.context().timer().unwrap().throttle(SECOND, true, move |()| callback()).unwrap();
    throttled.call(());
    assert_eq!(n(&calls), 1);
    advance(600).await;
    throttled.call(());
    assert_eq!(n(&calls), 1);
    advance(600).await;
    throttled.call(());
    assert_eq!(n(&calls), 2);
    advance(2000).await;
    assert_eq!(n(&calls), 3);
}

#[tokio::test(start_paused = true)]
async fn throttle_trailing_and_no_trailing() {
    let (_, fiber) = with_context().await;
    let timer = fiber.context().timer().unwrap();
    let (calls, callback) = counter();
    let throttled = timer.throttle(SECOND, true, move |()| callback()).unwrap();
    throttled.call(());
    advance(500).await;
    throttled.call(());
    assert_eq!(n(&calls), 1);
    advance(500).await;
    assert_eq!(n(&calls), 2);
    advance(2000).await;
    assert_eq!(n(&calls), 2);

    let (calls, callback) = counter();
    let throttled = timer.throttle(SECOND, false, move |()| callback()).unwrap();
    throttled.call(());
    advance(500).await;
    throttled.call(());
    advance(2000).await;
    assert_eq!(n(&calls), 1);
    throttled.call(());
    assert_eq!(n(&calls), 2);
}

#[tokio::test(start_paused = true)]
async fn throttle_stops_with_its_context_or_disposal() {
    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let throttled = fiber.context().timer().unwrap().throttle(SECOND, true, move |()| callback()).unwrap();
    throttled.call(());
    advance(500).await;
    throttled.call(());
    fiber.dispose().await.unwrap();
    advance(2000).await;
    throttled.call(());
    assert_eq!(n(&calls), 1);

    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let throttled = fiber.context().timer().unwrap().throttle(SECOND, false, move |()| callback()).unwrap();
    throttled.dispose().await.unwrap();
    throttled.call(());
    advance(2000).await;
    throttled.call(());
    assert_eq!(n(&calls), 0);
}

#[tokio::test(start_paused = true)]
async fn throttle_disposed() {
    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let throttled = fiber.context().timer().unwrap().throttle(SECOND, true, move |()| callback()).unwrap();
    throttled.dispose().await.unwrap();
    throttled.call(());
    assert_eq!(n(&calls), 0);
    advance(500).await;
    throttled.call(());
    advance(2000).await;
    assert_eq!(n(&calls), 0);
}

#[tokio::test(start_paused = true)]
async fn debounce() {
    let (_, fiber) = with_context().await;
    let timer = fiber.context().timer().unwrap();
    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let s = seen.clone();
    let debounced = timer.debounce(SECOND, move |value: u32| s.lock().push(value)).unwrap();
    debounced.call(1);
    advance(400).await;
    debounced.call(2);
    advance(400).await;
    debounced.call(3);
    assert!(seen.lock().is_empty());
    advance(1000).await;
    assert_eq!(*seen.lock(), vec![3]);

    debounced.dispose().await.unwrap();
    debounced.call(4);
    advance(2000).await;
    assert_eq!(*seen.lock(), vec![3]);
}

#[tokio::test(start_paused = true)]
async fn debounce_stops_with_its_context() {
    let (_, fiber) = with_context().await;
    let (calls, callback) = counter();
    let debounced = fiber.context().timer().unwrap().debounce(SECOND, move |()| callback()).unwrap();
    debounced.call(());
    fiber.dispose().await.unwrap();
    advance(2000).await;
    assert_eq!(n(&calls), 0);
    debounced.call(());
    advance(2000).await;
    assert_eq!(n(&calls), 0);
}
