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

fn make_structural_commit_fixture(
    sibling_count: usize,
    batch_size: usize,
    moving: bool,
) -> (UiModel, CommitAdapter, FrameworkCommit) {
    let mut ui = UiModel::new();
    let mut adapter = CommitAdapter::new();
    let mut initial = FrameworkCommit::new();
    initial.push(FrameworkOperation::CreateRootDeque {
        id: FrameworkDequeId(1),
    });
    initial.push(FrameworkOperation::CreateChildDeque {
        id: FrameworkDequeId(2),
        parent: FrameworkDequeId(1),
        side: anvil::deque_tree::ChildSide::Left,
    });
    initial.push(FrameworkOperation::CreateElement {
        id: FrameworkElementId(1),
        deque: FrameworkDequeId(1),
        tag: "main".into(),
        end: End::Back,
    });
    for index in 0..sibling_count {
        initial.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(10_000 + index as u64),
            deque: FrameworkDequeId(1),
            tag: "span".into(),
            end: End::Front,
        });
    }
    adapter.apply_commit(initial, &mut ui).unwrap();
    ui.prepare_render();

    let mut commit = FrameworkCommit::new();
    for index in 0..batch_size {
        let anchor = FrameworkElementId(10_000 + index as u64);
        if moving {
            commit.push(FrameworkOperation::MoveSubtree {
                anchor,
                target: FrameworkDequeId(2),
                target_end: End::Back,
            });
        } else {
            commit.push(FrameworkOperation::RemoveSubtree { anchor });
        }
    }
    (ui, adapter, commit)
}

fn make_multi_source_structural_fixture(
    sibling_count: usize,
    batch_size: usize,
    moving: bool,
) -> (UiModel, CommitAdapter, FrameworkCommit) {
    let mut ui = UiModel::new();
    let mut adapter = CommitAdapter::new();
    let mut setup = FrameworkCommit::new();
    setup.push(FrameworkOperation::CreateRootDeque {
        id: FrameworkDequeId(1),
    });
    setup.push(FrameworkOperation::CreateChildDeque {
        id: FrameworkDequeId(2),
        parent: FrameworkDequeId(1),
        side: anvil::deque_tree::ChildSide::Left,
    });
    setup.push(FrameworkOperation::CreateChildDeque {
        id: FrameworkDequeId(3),
        parent: FrameworkDequeId(1),
        side: anvil::deque_tree::ChildSide::Right,
    });

    let per_source = sibling_count / 2;
    for (source, deque) in [(0u64, 1u64), (1u64, 2u64)] {
        let base = 10_000 + source * 100_000;
        for index in 0..per_source {
            setup.push(FrameworkOperation::CreateElement {
                id: FrameworkElementId(base + index as u64),
                deque: FrameworkDequeId(deque),
                tag: "span".into(),
                end: End::Front,
            });
        }
        setup.push(FrameworkOperation::CreateElement {
            id: FrameworkElementId(base + per_source as u64),
            deque: FrameworkDequeId(deque),
            tag: "main".into(),
            end: End::Back,
        });
    }
    setup.push(FrameworkOperation::CreateElement {
        id: FrameworkElementId(900_000),
        deque: FrameworkDequeId(3),
        tag: "aside".into(),
        end: End::Back,
    });
    adapter.apply_commit(setup, &mut ui).unwrap();
    ui.prepare_render();

    let mut commit = FrameworkCommit::new();
    let per_source_batch = batch_size / 2;
    for source in 0..2u64 {
        let base = 10_000 + source * 100_000;
        for index in 0..per_source_batch {
            let anchor = FrameworkElementId(base + index as u64 + 1);
            if moving {
                commit.push(FrameworkOperation::MoveSubtree {
                    anchor,
                    target: FrameworkDequeId(3),
                    target_end: End::Back,
                });
            } else {
                commit.push(FrameworkOperation::RemoveSubtree { anchor });
            }
        }
    }
    (ui, adapter, commit)
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

    println!("Framework structural commits (including validation and UI bookkeeping):");
    for moving in [false, true] {
        for sibling_count in [128usize, 1_024, 4_096] {
            for batch_size in [1usize, 8, 32] {
                report_batch(
                    &format!(
                        "FrameworkCommit {} / siblings={sibling_count} batch={batch_size}",
                        if moving { "move" } else { "remove" }
                    ),
                    5,
                    || make_structural_commit_fixture(sibling_count, batch_size, moving),
                );
            }
            println!();
        }
    }

    println!(
        "Framework multi-source structural commits (two source Deques, same target for moves):"
    );
    for moving in [false, true] {
        for sibling_count in [128usize, 1_024, 4_096] {
            for batch_size in [8usize, 32] {
                report_batch(
                    &format!(
                        "FrameworkCommit multi-source {} / siblings={sibling_count} batch={batch_size}",
                        if moving { "move" } else { "remove" }
                    ),
                    5,
                    || make_multi_source_structural_fixture(sibling_count, batch_size, moving),
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
