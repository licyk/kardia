//! Port of `packages/core/tests/dispose.spec.ts` (effects on the root fiber).

mod common;

use common::{Seq, advance, settle, sleep};
use kardia_core::{Context, EffectMeta, Error};

fn meta(label: &str, children: Vec<EffectMeta>) -> EffectMeta {
    EffectMeta::new(label, children)
}

#[tokio::test]
async fn a_leaked_effect_outlives_its_fiber_until_the_caller_disposes_it() {
    let root = Context::new();
    let seq = Seq::new();
    let handle = root.on_dispose(seq.pusher(1)).unwrap();
    assert!(handle.leak(), "the effect was registered on the fiber");
    assert!(!handle.leak(), "a leaked effect is registered nowhere");

    // Disposing the fiber no longer reverts it.
    root.fiber().dispose().await.unwrap();
    assert_eq!(seq.len(), 0);

    // The caller owns it now.
    handle.dispose().await.unwrap();
    assert_eq!(seq.len(), 1);
}

#[tokio::test]
async fn dispose_manually() {
    let root = Context::new();
    let seq = Seq::new();
    let handle = root.on_dispose(seq.pusher(1)).unwrap();
    assert_eq!(root.fiber().effects(), vec![meta("ctx.on_dispose()", vec![])]);
    assert_eq!(seq.len(), 0);
    let disposal = handle.dispose();
    assert!(disposal.is_ready());
    assert_eq!(seq.len(), 1);
    handle.dispose().await.unwrap();
    assert_eq!(seq.len(), 1);
    assert_eq!(root.fiber().effects(), vec![]);
}

#[tokio::test]
async fn yield_dispose() {
    let root = Context::new();
    let seq = Seq::new();
    let handle = root
        .effect_sync("anonymous", |scope| {
            scope.defer_fn(seq.pusher(1));
            scope.adopt(root.on_dispose(|| {})?);
            scope.defer_fn(seq.pusher(2));
            scope.adopt(root.effect_sync("anonymous", |scope| {
                scope.adopt(root.on_dispose(|| {})?);
                scope.defer_fn(seq.pusher(3));
                Ok(())
            })?);
            Ok(())
        })
        .unwrap();
    root.on_dispose(|| {}).unwrap();
    assert_eq!(
        root.fiber().effects(),
        vec![
            meta(
                "anonymous",
                vec![
                    // only root level effects are listed; nested ones are children
                    meta("ctx.on_dispose()", vec![]),
                    meta("anonymous", vec![meta("ctx.on_dispose()", vec![])]),
                ],
            ),
            meta("ctx.on_dispose()", vec![]),
        ]
    );
    assert_eq!(seq.get(), Vec::<i32>::new());
    handle.dispose().await.unwrap();
    assert_eq!(seq.get(), vec![3, 2, 1]);
    handle.dispose().await.unwrap();
    assert_eq!(seq.get(), vec![3, 2, 1]);
    assert_eq!(root.fiber().effect_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn async_return_1() {
    let root = Context::new();
    let seq = Seq::new();
    let s = seq.clone();
    let handle = root
        .effect("anonymous", |scope| async move {
            sleep(100).await;
            s.push(1);
            scope.defer_fn(s.pusher(2));
            Ok(())
        })
        .unwrap();
    assert_eq!(seq.get(), Vec::<i32>::new());
    advance(100).await;
    assert_eq!(seq.get(), vec![1]);
    handle.dispose().await.unwrap();
    assert_eq!(seq.get(), vec![1, 2]);
}

#[tokio::test(start_paused = true)]
async fn async_return_2() {
    let root = Context::new();
    let seq = Seq::new();
    let s = seq.clone();
    let handle = root
        .effect("anonymous", |scope| async move {
            sleep(100).await;
            s.push(1);
            scope.defer_fn(s.pusher(2));
            Ok(())
        })
        .unwrap();
    drop(handle.dispose());
    assert_eq!(seq.get(), Vec::<i32>::new());
    advance(100).await;
    assert_eq!(seq.get(), vec![1, 2]);
}

fn three_steps(root: &Context, seq: &Seq<i32>) -> kardia_core::EffectHandle {
    let s = seq.clone();
    root.effect("anonymous", |scope| async move {
        sleep(100).await;
        s.push(1);
        scope.yield_(kardia_core::Disposer::new(s.pusher(2)))?;
        sleep(100).await;
        s.push(3);
        scope.yield_(kardia_core::Disposer::new(s.pusher(4)))?;
        sleep(100).await;
        s.push(5);
        scope.yield_(kardia_core::Disposer::new(s.pusher(6)))?;
        Ok(())
    })
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn async_yield_1() {
    let root = Context::new();
    let seq = Seq::new();
    let handle = three_steps(&root, &seq);
    assert_eq!(seq.get(), Vec::<i32>::new());
    advance(300).await;
    assert_eq!(seq.get(), vec![1, 3, 5]);
    handle.dispose().await.unwrap();
    assert_eq!(seq.get(), vec![1, 3, 5, 6, 4, 2]);
}

#[tokio::test(start_paused = true)]
async fn async_yield_2_aborted() {
    let root = Context::new();
    let seq = Seq::new();
    let handle = three_steps(&root, &seq);
    advance(50).await;
    drop(handle.dispose());
    assert_eq!(seq.get(), Vec::<i32>::new());
    advance(300).await;
    assert_eq!(seq.get(), vec![1, 2]);
}

#[tokio::test(start_paused = true)]
async fn async_yield_3_aborted() {
    let root = Context::new();
    let seq = Seq::new();
    let handle = three_steps(&root, &seq);
    assert_eq!(seq.get(), Vec::<i32>::new());
    advance(100).await;
    assert_eq!(seq.get(), vec![1]);
    drop(handle.dispose());
    assert_eq!(seq.get(), vec![1]);
    advance(200).await;
    assert_eq!(seq.get(), vec![1, 3, 4, 2]);
}

struct Finally(Seq<i32>);

impl Drop for Finally {
    fn drop(&mut self) {
        self.0.push(0);
    }
}

#[tokio::test(start_paused = true)]
async fn async_yield_aborted_finally() {
    let root = Context::new();
    let seq = Seq::new();
    let s = seq.clone();
    let handle = root
        .effect("anonymous", |scope| async move {
            let _finally = Finally(s.clone());
            scope.yield_(kardia_core::Disposer::new(s.pusher(1)))?;
            sleep(100).await;
            scope.yield_(kardia_core::Disposer::new(s.pusher(2)))?;
            sleep(100).await;
            scope.yield_(kardia_core::Disposer::new(s.pusher(3)))?;
            Ok(())
        })
        .unwrap();
    advance(50).await;
    let disposing = handle.dispose();
    advance(100).await;
    disposing.await.unwrap();
    assert_eq!(seq.get(), vec![0, 2, 1]);
}

#[tokio::test(start_paused = true)]
async fn async_yield_4_await_ready() {
    let root = Context::new();
    let seq = Seq::new();
    let handle = three_steps(&root, &seq);
    assert_eq!(seq.get(), Vec::<i32>::new());
    let (ready, _) = tokio::join!(handle.ready(), advance(300));
    ready.unwrap();
    assert_eq!(seq.get(), vec![1, 3, 5]);
    handle.dispose().await.unwrap();
    assert_eq!(seq.get(), vec![1, 3, 5, 6, 4, 2]);
}

#[tokio::test]
async fn return_with_error() {
    let root = Context::new();
    let seq = Seq::<i32>::new();
    let result = root.effect_sync("anonymous", |_| Err(Error::msg("test")));
    assert_eq!(result.err().unwrap().to_string(), "test");
    assert_eq!(seq.get(), Vec::<i32>::new());
    assert_eq!(root.fiber().effect_count(), 0);
}

#[tokio::test]
async fn yield_with_error() {
    let root = Context::new();
    let seq = Seq::new();
    let result = root.effect_sync("anonymous", |scope| {
        scope.defer_fn(seq.pusher(1));
        Err(Error::msg("test"))
    });
    assert_eq!(result.err().unwrap().to_string(), "test");
    assert_eq!(seq.get(), vec![1]);
}

#[tokio::test]
async fn panic_in_body_is_an_error() {
    let root = Context::new();
    let seq = Seq::new();
    let result = root.effect_sync("anonymous", |scope| {
        scope.defer_fn(seq.pusher(1));
        panic!("boom");
    });
    assert!(result.err().unwrap().to_string().contains("boom"));
    assert_eq!(seq.get(), vec![1]);
}

#[tokio::test]
async fn async_return_with_error() {
    let root = Context::new();
    let handle = root.effect("anonymous", |_| async { Err(Error::msg("test")) }).unwrap();
    assert_eq!(handle.ready().await.unwrap_err().to_string(), "test");
    let errors: Vec<_> = root.log_buffer().into_iter().map(|m| m.text).collect();
    assert_eq!(errors, vec!["test"]);
}

#[tokio::test]
async fn async_yield_with_error() {
    let root = Context::new();
    let seq = Seq::new();
    let s = seq.clone();
    let handle = root
        .effect("anonymous", |scope| async move {
            scope.yield_(kardia_core::Disposer::new(s.pusher(1)))?;
            Err(Error::msg("test"))
        })
        .unwrap();
    assert!(handle.ready().await.is_err());
    assert_eq!(seq.get(), vec![1]);
    handle.dispose().await.unwrap();
    assert_eq!(seq.get(), vec![1]);
}

#[tokio::test]
async fn disposers_run_in_reverse_and_survive_failures() {
    let root = Context::new();
    let seq = Seq::new();
    let s = seq.clone();
    let handle = root
        .effect_sync("anonymous", |scope| {
            scope.defer_fn(seq.pusher(1));
            scope.defer(kardia_core::Disposer::future(move || async move {
                settle().await;
                s.push(2);
            }));
            scope.defer(kardia_core::Disposer::fallible(|| Err(Error::msg("failed"))));
            scope.defer_fn(seq.pusher(3));
            Ok(())
        })
        .unwrap();
    let disposal = handle.dispose();
    assert!(!disposal.is_ready());
    assert_eq!(seq.get(), vec![3]);
    assert_eq!(disposal.await.unwrap_err().to_string(), "failed");
    assert_eq!(seq.get(), vec![3, 2, 1]);
}

#[tokio::test]
async fn repeated_disposal_waits_for_cleanup_and_shares_failure() {
    use futures::FutureExt;
    use kardia_core::Disposer;
    for async_setup in [false, true] {
        let root = Context::new();
        let seq = Seq::new();
        let calls = seq.clone();
        let error = Error::msg("cleanup failed");
        let failure = error.clone();
        let (release, gate) = tokio::sync::oneshot::channel();
        let disposer = Disposer::try_future(move || {
            calls.push(1);
            async move {
                gate.await.unwrap();
                Err(failure)
            }
        });
        let (setup_release, setup_gate) = tokio::sync::oneshot::channel();
        let handle = if async_setup {
            root.effect("pending setup", |scope| async move {
                setup_gate.await.unwrap();
                scope.defer(disposer);
                Ok(())
            })
            .unwrap()
        } else {
            root.defer("sync setup", disposer).unwrap()
        };
        let first = handle.dispose();
        let second = handle.dispose();
        assert!(!second.is_ready());
        let mut second = Box::pin(second);
        assert!(second.as_mut().now_or_never().is_none());
        if async_setup {
            setup_release.send(()).unwrap();
            handle.ready().await.unwrap();
            assert!(second.as_mut().now_or_never().is_none());
        }
        drop(first);
        release.send(()).unwrap();
        assert!(second.await.unwrap_err().ptr_eq(&error));
        assert!(handle.dispose().await.unwrap_err().ptr_eq(&error));
        assert_eq!(seq.get(), vec![1]);
        root.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn repeated_sync_disposal_preserves_error_and_immediate_completion() {
    let root = Context::new();
    let error = Error::msg("sync cleanup failed");
    let failure = error.clone();
    let handle = root.defer("sync", kardia_core::Disposer::fallible(move || Err(failure))).unwrap();
    let first = handle.dispose();
    assert!(first.is_ready());
    assert!(first.await.unwrap_err().ptr_eq(&error));
    let second = handle.dispose();
    assert!(second.is_ready());
    assert!(second.await.unwrap_err().ptr_eq(&error));
}
