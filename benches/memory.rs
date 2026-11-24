use anvil::deque_tree::DequeRegistry;
use anvil::runtime::{
    DequeHandle, ElementHandle, InitialDeque, InitialElement, UiElement, UiModel,
};
use anvil::{Deque, DequeTree};
use std::fs;
use std::hint::black_box;

fn resident_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?;
        value.split_whitespace().next()?.parse().ok()
    })
}

fn measure_tree(size: usize) {
    let before = resident_kib();
    let mut deques = Vec::with_capacity(size);
    for index in 0..size {
        let mut deque = Deque::new();
        deque.push_back(index);
        deques.push(deque);
    }
    let mut tree = DequeTree::from_level_order(deques).expect("valid level-order topology");
    let registry = DequeRegistry::build(&mut tree);
    black_box((&tree, &registry));
    report("DequeTree + DequeRegistry", size, before, resident_kib());
}

fn measure_ui(size: usize) {
    let before = resident_kib();
    let mut elements = Vec::with_capacity(size + 1);
    for index in 0..size {
        elements.push(InitialElement {
            handle: ElementHandle(index as u64 + 1),
            element: UiElement::new("div"),
            subtree_span: 1,
        });
    }
    elements.push(InitialElement {
        handle: ElementHandle(size as u64 + 1),
        element: UiElement::new("main"),
        subtree_span: size + 1,
    });
    let ui = UiModel::from_initial_groups(vec![InitialDeque {
        handle: DequeHandle(1),
        elements,
    }])
    .expect("valid initial UI snapshot");
    black_box(&ui);
    report(
        "UiModel with one flat sibling list",
        size,
        before,
        resident_kib(),
    );
}

fn report(label: &str, size: usize, before: Option<u64>, after: Option<u64>) {
    println!("memory footprint (Linux VmRSS; approximate, includes allocator/runtime overhead)");
    println!("fixture={label} items={size}");
    match (before, after) {
        (Some(before), Some(after)) => println!(
            "before={before} KiB after={after} KiB delta={} KiB",
            after.saturating_sub(before)
        ),
        _ => println!("VmRSS unavailable on this platform"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "tree".into());
    let size = args
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2_047);

    match mode.as_str() {
        "tree" => measure_tree(size),
        "ui" => measure_ui(size),
        _ => {
            eprintln!("usage: cargo bench --bench memory -- [tree|ui] [item-count]");
            std::process::exit(2);
        }
    }
}
