//! What diverged from the recording, per sandbox and invocation.

use crate::control_plane::InvocationResult;
use bottlecap::replay::format::ResultKind;
use bottlecap::replay::matcher::{self, Entry};
use bottlecap::replay::player::Logged;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct NearMiss {
    pub recorded_seq: u64,
    pub differences: Vec<String>,
}

impl From<matcher::NearMiss> for NearMiss {
    fn from(m: matcher::NearMiss) -> Self {
        Self {
            recorded_seq: m.seq,
            differences: m.differences,
        }
    }
}

/// A request the function made that the recording has no answer for.
#[derive(Clone, Debug, Serialize)]
pub struct Unmatched {
    #[serde(skip)]
    pub request_id: Option<String>,
    pub method: String,
    pub url: String,
    pub operation: Option<String>,
    pub near_miss: Option<NearMiss>,
}

impl From<&Logged> for Unmatched {
    fn from(l: &Logged) -> Self {
        Self {
            request_id: l.request_id.clone(),
            method: l.request.method.clone(),
            url: l.request.url.clone(),
            operation: matcher::operation(&l.request),
            near_miss: l.near_miss.clone().map(Into::into),
        }
    }
}

/// A recorded request the function did not make.
#[derive(Clone, Debug, Serialize)]
pub struct Missing {
    pub seq: u64,
    pub method: String,
    pub url: String,
    pub operation: Option<String>,
}

impl From<&Entry> for Missing {
    fn from(e: &Entry) -> Self {
        Self {
            seq: e.seq,
            method: e.request.method.clone(),
            url: e.request.url.clone(),
            operation: matcher::operation(&e.request),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct InvocationReport {
    pub request_id: String,
    pub served: usize,
    pub unmatched: Vec<Unmatched>,
    /// `--update`: calls not in the recording, sent upstream and added to it.
    pub added: Vec<Unmatched>,
    pub missing: Vec<Missing>,
    pub response_differences: Vec<String>,
    pub errors: Vec<String>,
}

impl InvocationReport {
    pub fn new(request_id: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            served: 0,
            unmatched: Vec::new(),
            added: Vec::new(),
            missing: Vec::new(),
            response_differences: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn compare_result(
        &mut self,
        kind: ResultKind,
        recorded: &[u8],
        actual: &InvocationResult,
        ignored: &[&str],
    ) {
        if kind != actual.kind {
            self.response_differences.push(format!(
                "recorded {kind:?}, replay returned {:?}",
                actual.kind
            ));
        }
        self.response_differences
            .extend(matcher::body_differences_ignoring(
                recorded,
                &actual.body,
                ignored,
            ));
    }

    pub fn passed(&self) -> bool {
        self.unmatched.is_empty()
            && self.missing.is_empty()
            && self.response_differences.is_empty()
            && self.errors.is_empty()
    }
}

#[derive(Debug, Serialize)]
pub struct SessionReport {
    pub sandbox_id: String,
    pub invocations: Vec<InvocationReport>,
    pub init_unmatched: Vec<Unmatched>,
    pub init_added: Vec<Unmatched>,
    pub init_missing: Vec<Missing>,
    pub errors: Vec<String>,
    pub intake_requests: usize,
}

impl SessionReport {
    pub fn new(sandbox_id: &str) -> Self {
        Self {
            sandbox_id: sandbox_id.to_string(),
            invocations: Vec::new(),
            init_unmatched: Vec::new(),
            init_added: Vec::new(),
            init_missing: Vec::new(),
            errors: Vec::new(),
            intake_requests: 0,
        }
    }

    pub fn passed(&self) -> bool {
        self.errors.is_empty()
            && self.init_unmatched.is_empty()
            && self.init_missing.is_empty()
            && self.invocations.iter().all(InvocationReport::passed)
    }
}

fn op(operation: Option<&String>) -> String {
    operation.map(|o| format!(" ({o})")).unwrap_or_default()
}

fn print_calls(indent: &str, unmatched: &[Unmatched], missing: &[Missing]) {
    for u in unmatched {
        println!(
            "{indent}+ unexpected {} {}{}",
            u.method,
            u.url,
            op(u.operation.as_ref())
        );
        if let Some(m) = &u.near_miss {
            println!("{indent}    closest recorded call is #{}:", m.recorded_seq);
            for d in &m.differences {
                println!("{indent}      {d}");
            }
        }
    }
    for m in missing {
        println!(
            "{indent}- missing    {} {}{} (recorded #{})",
            m.method,
            m.url,
            op(m.operation.as_ref()),
            m.seq
        );
    }
}

/// Prints the report; returns whether everything matched.
pub fn print(sessions: &[SessionReport]) -> bool {
    let mut all = true;
    for s in sessions {
        println!("\nsandbox {}", s.sandbox_id);
        for e in &s.errors {
            println!("  ! {e}");
        }
        if !s.init_unmatched.is_empty() || !s.init_missing.is_empty() {
            println!("  init / between invocations:");
            print_calls("    ", &s.init_unmatched, &s.init_missing);
        }
        for inv in &s.invocations {
            let mark = if inv.passed() { "PASS" } else { "FAIL" };
            println!(
                "  {mark} {}  ({} recorded calls served)",
                inv.request_id, inv.served
            );
            for e in &inv.errors {
                println!("      ! {e}");
            }
            if !inv.response_differences.is_empty() {
                println!("      response differs:");
                for d in &inv.response_differences {
                    println!("        {d}");
                }
            }
            print_calls("      ", &inv.unmatched, &inv.missing);
        }
        all &= s.passed();
    }
    let total: usize = sessions.iter().map(|s| s.invocations.len()).sum();
    let failed: usize = sessions
        .iter()
        .flat_map(|s| &s.invocations)
        .filter(|i| !i.passed())
        .count();
    println!(
        "\n{} invocations, {} failed{}",
        total,
        failed,
        if all { "" } else { " — replay diverged" }
    );
    all
}

/// Prints what `--update` changed; returns whether the run completed.
pub fn print_update(sessions: &[SessionReport]) -> bool {
    let mut ok = true;
    for s in sessions {
        println!("\nsandbox {} (updated)", s.sandbox_id);
        for e in &s.errors {
            println!("  ! {e}");
        }
        if !s.init_added.is_empty() || !s.init_missing.is_empty() {
            println!("  init / between invocations:");
            print_changes("    ", &s.init_added, &s.init_missing);
        }
        for inv in &s.invocations {
            let changed = !inv.added.is_empty()
                || !inv.missing.is_empty()
                || !inv.response_differences.is_empty();
            let mark = if changed { "CHANGED" } else { "same   " };
            println!(
                "  {mark} {}  ({} recorded calls reused)",
                inv.request_id, inv.served
            );
            for e in &inv.errors {
                println!("      ! {e}");
            }
            print_changes("      ", &inv.added, &inv.missing);
            if !inv.response_differences.is_empty() {
                println!("      response changed:");
                for d in &inv.response_differences {
                    println!("        {d}");
                }
            }
            ok &= inv.errors.is_empty();
        }
        ok &= s.errors.is_empty();
    }
    let added: usize = sessions
        .iter()
        .map(|s| s.init_added.len() + s.invocations.iter().map(|i| i.added.len()).sum::<usize>())
        .sum();
    let dropped: usize = sessions
        .iter()
        .map(|s| {
            s.init_missing.len() + s.invocations.iter().map(|i| i.missing.len()).sum::<usize>()
        })
        .sum();
    println!("\n{added} call(s) recorded from the real upstream, {dropped} dropped");
    ok
}

fn print_changes(indent: &str, added: &[Unmatched], dropped: &[Missing]) {
    for a in added {
        println!(
            "{indent}+ recorded {} {}{}",
            a.method,
            a.url,
            op(a.operation.as_ref())
        );
    }
    for d in dropped {
        println!(
            "{indent}- dropped  {} {}{} (was #{})",
            d.method,
            d.url,
            op(d.operation.as_ref()),
            d.seq
        );
    }
}
