//! Model-based tests of the metatheory
//! (<https://licyk.github.io/kardia/docs/development-testing#model-based>).
//!
//! Synthetic components write to a shared table and revert exactly; random
//! orchestration sequences run under random schedules (delays in loads and
//! inverses, interleavings between operations). At quiescence the runtime
//! must match the declarative model, whatever the schedule.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::Seq;
use kardia_core::{Context, Disposer, Fiber, FiberState, PluginDef};
use parking_lot::Mutex;
use proptest::prelude::*;
use serde_json::{Value, json};

/// Kind `i` provides `KEYS[i]` (if any) and injects only keys of lower
/// kinds, so the declared precedence relation is acyclic.
const KEYS: [Option<&str>; 5] = [Some("a"), Some("b"), None, Some("c"), None];

#[derive(Debug, Clone)]
enum Op {
    Toggle(usize),
    Update(usize),
    Yield(u8),
}

fn kinds() -> impl Strategy<Value = Vec<Vec<usize>>> {
    (0..KEYS.len())
        .map(|i| proptest::sample::subsequence((0..i).filter(|j| KEYS[*j].is_some()).collect::<Vec<_>>(), 0..=i.min(2)))
        .collect::<Vec<_>>()
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        prop_oneof![
            4 => (0..KEYS.len()).prop_map(Op::Toggle),
            1 => (0..KEYS.len()).prop_map(Op::Update),
            2 => (0..4u8).prop_map(Op::Yield),
        ],
        1..24,
    )
}

/// A provision: `alive` holds exactly during the provider's episode.
struct Token {
    alive: AtomicBool,
}

#[derive(Clone, Default)]
struct World {
    /// The shared table: kind → number of live writes.
    table: Arc<Mutex<BTreeMap<usize, usize>>>,
    violations: Seq<String>,
}

fn def(kind: usize, inject: &[usize], world: &World, delays: (u64, u64)) -> PluginDef {
    let world = world.clone();
    let inject_keys: Vec<&'static str> = inject.iter().map(|j| KEYS[*j].unwrap()).collect();
    let keys = inject_keys.clone();
    PluginDef::new(move |ctx, _| {
        let (world, keys) = (world.clone(), keys.clone());
        async move {
            tokio::time::sleep(Duration::from_millis(delays.0)).await;
            check_dependencies(&ctx, &keys, &world, "load");
            if let Some(key) = KEYS[kind] {
                let token = Arc::new(Token { alive: AtomicBool::new(true) });
                ctx.provide_arc_as(key, token.clone())?;
                ctx.on_dispose(move || token.alive.store(false, Ordering::SeqCst))?;
            }
            *world.table.lock().entry(kind).or_default() += 1;
            let (reader, table) = (ctx.clone(), world.table.clone());
            ctx.defer(
                "write",
                Disposer::future(move || async move {
                    tokio::time::sleep(Duration::from_millis(delays.1)).await;
                    check_dependencies(&reader, &keys, &world, "cleanup");
                    let mut table = table.lock();
                    let count = table.get_mut(&kind).expect("written by the load");
                    *count -= 1;
                    if *count == 0 {
                        table.remove(&kind);
                    }
                }),
            )?;
            Ok(())
        }
    })
    .name(format!("kind{kind}"))
    .inject(inject_keys)
    .provides(KEYS[kind])
}

/// Ordering (Thm 70): a consumer only reads bindings inside their provider's
/// episode, also during its own teardown (resolution through ω).
fn check_dependencies(ctx: &Context, keys: &[&str], world: &World, phase: &str) {
    for key in keys {
        match ctx.get_as::<Token>(*key) {
            Ok(token) if token.alive.load(Ordering::SeqCst) => {}
            Ok(_) => world.violations.push(format!("{phase}: {key} read outside its provider's episode")),
            Err(error) => world.violations.push(format!("{phase}: {key}: {error}")),
        }
    }
}

async fn quiesce(fibers: &[Option<Fiber>]) {
    for _ in 0..8 {
        for fiber in fibers.iter().flatten() {
            let _ = fiber.settled().await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn run(kinds: &[Vec<usize>], ops: &[Op], delays: &[(u64, u64)]) -> Result<(), TestCaseError> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().unwrap();
    runtime.block_on(async {
        let root = Context::new();
        let world = World::default();
        let defs: Vec<_> = (0..KEYS.len()).map(|i| def(i, &kinds[i], &world, delays[i])).collect();
        let mut fibers: Vec<Option<Fiber>> = vec![None; KEYS.len()];
        for op in ops {
            match op {
                Op::Toggle(i) => match fibers[*i].take() {
                    Some(fiber) => drop(fiber.dispose()),
                    None => fibers[*i] = Some(root.plugin_def(&defs[*i], Value::Null).unwrap()),
                },
                Op::Update(i) => {
                    if let Some(fiber) = &fibers[*i] {
                        drop(fiber.update(json!({ "op": format!("{op:?}") })).unwrap());
                    }
                }
                Op::Yield(n) => {
                    for _ in 0..*n {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
        quiesce(&fibers).await;

        // the support set: live kinds whose dependencies are all active (acyclic fixpoint)
        let mut active = BTreeSet::new();
        for i in 0..KEYS.len() {
            if fibers[i].is_some() && kinds[i].iter().all(|j| active.contains(j)) {
                active.insert(i);
            }
        }
        for (i, fiber) in fibers.iter().enumerate() {
            let Some(fiber) = fiber else { continue };
            let expected = if active.contains(&i) { FiberState::Active } else { FiberState::Pending };
            prop_assert_eq!(fiber.state(), expected, "kind {}", i);
        }
        // recovery exactness: the table holds exactly one write per active kind
        let table = world.table.lock().clone();
        prop_assert_eq!(table, active.iter().map(|i| (*i, 1)).collect::<BTreeMap<_, _>>());
        prop_assert_eq!(world.violations.get(), Vec::<String>::new());
        // preservation: only active providers keep live bindings
        for (i, key) in KEYS.iter().enumerate() {
            if let Some(key) = key {
                prop_assert_eq!(root.live_as::<Token>(*key).is_some(), active.contains(&i), "binding of {}", key);
            }
        }
        // retiring everything reverts every write
        root.fiber().dispose().await.unwrap();
        quiesce(&fibers).await;
        prop_assert!(world.table.lock().is_empty());
        prop_assert_eq!(root.registry().size(), 0);
        Ok(())
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn orchestrations_reach_the_declarative_model(
        kinds in kinds(),
        ops in ops(),
        delays in prop::collection::vec((0..3u64, 0..3u64), KEYS.len()),
    ) {
        run(&kinds, &ops, &delays)?;
    }
}
