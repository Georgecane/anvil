use anvil::deque_tree::End;
use anvil::runtime::{
    CommitAdapter, DequeHandle, ElementHandle, FrameworkAdapter, FrameworkCommit, FrameworkDequeId,
    FrameworkElementId, FrameworkOperation, InitialDeque, InitialElement, UiElement, UiModel,
};
use std::hint::black_box;
use std::time::{Duration, Instant};

fn make_fixture(sibling_count: usize, subtree_size: usize) -> (UiModel, ElementHandle) {
    let mut elements = Vec::with_capacity(sibling_count + subtree_size + 2);
    let mut next = 1u64;

    // Physical order is reverse preorder. Put half of the sibling roots before
    // the moved subtree so the anchor is in the middle of the source Deque.
    for _ in 0..sibling_count / 2 {
        elements.push(InitialElement {
            handle: ElementHandle(next),
            element: UiElement::new("span"),
            subtree_span: 1,
        });
        next += 1;
    }

    for _ in 0..subtree_size {
        elements.push(InitialElement {
            handle: ElementHandle(next),
            element: UiElement::new("i"),
            subtree_span: 1,
        });
        next += 1;
    }

    let anchor = ElementHandle(next);
    elements.push(InitialElement {
        handle: anchor,
        element: UiElement::new("section"),
        subtree_span: subtree_size + 1,
    });
    next += 1;

    for _ in sibling_count / 2..sibling_count {
        elements.push(InitialElement {
            handle: ElementHandle(next),
            element: UiElement::new("span"),
            subtree_span: 1,
        });
        next += 1;
    }

    let root_span = elements.len() + 1;
    elements.push(InitialElement {
        handle: ElementHandle(next),
        element: UiElement::new("main"),
        subtree_span: root_span,
    });

    let ui = UiModel::from_initial_groups(vec![
        InitialDeque {
            handle: DequeHandle(1),
            elements,
        },
        InitialDeque {
            handle: DequeHandle(2),
            elements: Vec::new(),
        },
    ])
    .expect("valid source/target topology");
    (ui, anchor)
}

fn report_fresh(
    label: &str,
    iterations: usize,
    mut setup: impl FnMut() -> (UiModel, ElementHandle),
    mut operation: impl FnMut(&mut UiModel, ElementHandle),
) {
    for _ in 0..5 {
        let (mut ui, handle) = setup();
        operation(&mut ui, handle);
    }

    let mut samples = Vec::with_capacity(7);
    for _ in 0..7 {
        let mut elapsed = Duration::ZERO;
        for _ in 0..iterations {
            let (mut ui, handle) = setup();
            let start = Instant::now();
            operation(&mut ui, handle);
            elapsed += start.elapsed();
            black_box(ui.element(handle));
        }
        samples.push(elapsed);
    }
    samples.sort_unstable();
    let median = samples[3].as_secs_f64() * 1e9 / iterations as f64;
    let best = samples[0].as_secs_f64() * 1e9 / iterations as f64;
    println!("{label:58} median={median:>11.2} ns/op  best={best:>11.2} ns/op");
}

fn make_batch_fixture(
    sibling_count: usize,
    batch_size: usize,
    end: End,
) -> (UiModel, CommitAdapter, FrameworkCommit) {
    let mut elements = Vec::with_capacity(sibling_count + 1);
    for index in 0..sibling_count {
        elements.push(InitialElement {
            handle: ElementHandle(index as u64 + 1),
            element: UiElement::new("span"),
            subtree_span: 1,
        });
    }
    elements.push(InitialElement {
        handle: ElementHandle(sibling_count as u64 + 1),
        element: UiElement::new("main"),
        subtree_span: sibling_count + 1,
    });

    let ui = UiModel::from_initial_groups(vec![InitialDeque {
        handle: DequeHandle(1),
        elements,
    }])
    .expect("valid initial UI snapshot");
    let mut adapter = CommitAdapter::new();
    adapter
        .bind_existing_root(FrameworkDequeId(1), &ui)
        .expect("bind the existing root");

    let mut commit = FrameworkCommit::new();
    for index in 0..batch_size {
        commit.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(100_000 + index as u64),
            deque: FrameworkDequeId(1),
            tag: "div".into(),
            end,
        });
    }
    (ui, adapter, commit)
}

fn report_batch(
    label: &str,
    iterations: usize,
    mut setup: impl FnMut() -> (UiModel, CommitAdapter, FrameworkCommit),
) {
    for _ in 0..3 {
        let (mut ui, mut adapter, commit) = setup();
        adapter.apply_commit(commit, &mut ui).unwrap();
    }

    let mut samples = Vec::with_capacity(7);
    for _ in 0..7 {
        let mut elapsed = Duration::ZERO;
        for _ in 0..iterations {
            let (mut ui, mut adapter, commit) = setup();
            let start = Instant::now();
            adapter.apply_commit(commit, &mut ui).unwrap();
            elapsed += start.elapsed();
            black_box(ui.tree().len());
        }
        samples.push(elapsed);
    }
    samples.sort_unstable();
    let median = samples[3].as_secs_f64() * 1e9 / iterations as f64;
    let best = samples[0].as_secs_f64() * 1e9 / iterations as f64;
    println!("{label:58} median={median:>11.2} ns/commit  best={best:>11.2} ns/commit");
}

fn main() {
    println!("Anvil structural mutation benchmarks");
    println!("target={} profile=bench", std::env::consts::ARCH);
    println!("Seven samples; fixture setup excluded from each timed operation.");
    println!("Each sample runs fresh fixtures to preserve the same interior position.");
    println!("Move includes mutation journal and targeted Registry bookkeeping.");
    println!();

    for sibling_count in [128usize, 1_024, 4_096] {
        for subtree_size in [1usize, 16, 256] {
            report_fresh(
                &format!("UiModel.move_subtree / siblings={sibling_count} subtree={subtree_size}"),
                20,
                || make_fixture(sibling_count, subtree_size),
                |ui, anchor| {
                    ui.move_subtree(anchor, DequeHandle(2), End::Back).unwrap();
                },
            );
        }
        println!();
    }

    println!("Framework batch insertion (validation and per-operation UI/Registry work included):");
    for end in [End::Front, End::Back] {
        for sibling_count in [128usize, 1_024, 4_096] {
            for batch_size in [1usize, 8, 32] {
                report_batch(
                    &format!(
                        "FrameworkCommit create / end={end:?} siblings={sibling_count} batch={batch_size}"
                    ),
                    10,
                    || make_batch_fixture(sibling_count, batch_size, end),
                );
            }
            println!();
        }
    }

    for sibling_count in [128usize, 1_024, 4_096] {
        report_fresh(
            &format!("UiModel.remove_subtree / siblings={sibling_count}"),
            20,
            || make_fixture(sibling_count, 1),
            |ui, anchor| {
                black_box(ui.remove_subtree(anchor).unwrap());
            },
        );
        println!();
    }
}
