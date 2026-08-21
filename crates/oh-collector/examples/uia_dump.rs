//! Dumps the UIAutomation tree of a top-level window, for working out what a given
//! application actually exposes.
//!
//! ```text
//! cargo run -p oh-collector --example uia_dump -- "about:blank" 4
//! ```
//!
//! The first argument matches a window title, case-insensitively, as a substring.
//! The second is how deep to walk. Kept in the repository because browser vendors
//! change their accessibility trees, and re-deriving how to read one is slow.

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Children,
};

const MAX_NODES: usize = 600;

fn main() -> anyhow::Result<()> {
    let needle = std::env::args()
        .nth(1)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let depth: usize = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(4);

    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;

    let root = unsafe { automation.GetRootElement() }?;
    let condition = unsafe { automation.CreateTrueCondition() }?;
    let windows = unsafe { root.FindAll(TreeScope_Children, &condition) }?;

    let mut budget = MAX_NODES;
    for i in 0..unsafe { windows.Length() }? {
        let window = unsafe { windows.GetElement(i) }?;
        let name = unsafe { window.CurrentName() }
            .map(|n| n.to_string())
            .unwrap_or_default();
        if needle.is_empty() || name.to_ascii_lowercase().contains(&needle) {
            println!("== {name} ==");
            walk(&automation, &window, 0, depth, &mut budget)?;
        }
    }

    if budget == 0 {
        eprintln!("(output truncated at {MAX_NODES} nodes)");
    }
    Ok(())
}

fn walk(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
    level: usize,
    max: usize,
    budget: &mut usize,
) -> anyhow::Result<()> {
    if level >= max || *budget == 0 {
        return Ok(());
    }

    let condition = unsafe { automation.CreateTrueCondition() }?;
    let Ok(children) = (unsafe { element.FindAll(TreeScope_Children, &condition) }) else {
        return Ok(());
    };

    for i in 0..unsafe { children.Length() }? {
        if *budget == 0 {
            return Ok(());
        }
        *budget -= 1;

        let Ok(child) = (unsafe { children.GetElement(i) }) else {
            continue;
        };
        let name = unsafe { child.CurrentName() }
            .map(|n| n.to_string())
            .unwrap_or_default();
        let class = unsafe { child.CurrentClassName() }
            .map(|n| n.to_string())
            .unwrap_or_default();
        let id = unsafe { child.CurrentAutomationId() }
            .map(|n| n.to_string())
            .unwrap_or_default();
        let control = unsafe { child.CurrentControlType() }
            .map(|c| c.0)
            .unwrap_or_default();

        println!(
            "{:indent$}[{control}] name={name:?} class={class:?} id={id:?}",
            "",
            indent = level * 2
        );

        walk(automation, &child, level + 1, max, budget)?;
    }
    Ok(())
}
