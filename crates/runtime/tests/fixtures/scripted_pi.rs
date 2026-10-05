//! Deterministic RPC subprocess for tests. No provider, environment secrets, or Pi needed.
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    path::Path,
    time::Duration,
};

fn emit(value: &Value) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{value}")?;
    stdout.flush()
}

fn assistant(text: &str, reason: &str) -> Value {
    json!({"type":"message_end","message":{
        "role":"assistant","stopReason":reason,
        "content":[{"type":"thinking","thinking":"never expose this"},{"type":"text","text":text}]
    }})
}

fn wait_for_release() {
    while !Path::new("release").exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write("pid", std::process::id().to_string())?;
    std::fs::write(
        "args",
        serde_json::to_string(&std::env::args().skip(1).collect::<Vec<_>>())?,
    )?;
    let mode = std::fs::read_to_string("mode")?;
    let mut lines = io::stdin().lock().lines();
    let prompt = lines.next().ok_or("stdin closed before prompt")??;
    let prompt: Value = serde_json::from_str(&prompt)?;
    std::fs::write("prompt", prompt.to_string())?;
    if mode == "exit" {
        return Ok(());
    }
    if mode == "malformed" {
        println!("this is not JSON");
    } else if mode == "oversized" {
        println!("{}", "x".repeat(8 * 1024 * 1024 + 1));
    } else if mode == "protocol_error" {
        emit(
            &json!({"id":prompt.get("id"),"type":"response","command":"prompt","success":false,"error":"sensitive diagnostic"}),
        )?;
    } else if mode == "handled" {
        emit(
            &json!({"id":prompt.get("id"),"type":"response","command":"prompt","success":true,"data":{"disposition":"handled"}}),
        )?;
    } else {
        // Deliberately interleave events and responses; stderr is invalid JSON.
        eprintln!("diagnostics are not protocol data");
        if mode == "stderr_flood" {
            eprintln!("{}", "diagnostics".repeat(100_000));
        }
        emit(&json!({"type":"agent_start"}))?;
        if mode == "fast" {
            finish_run(&mode)?;
        }
        emit(
            &json!({"id":prompt.get("id"),"type":"response","command":"prompt","success":true,"data":{"disposition":"started"}}),
        )?;
        emit(
            &json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"intermediate text must not be the result"}}),
        )?;
        emit(&json!({"type":"tool_execution_start","toolName":"bash"}))?;
        emit(&json!({"type":"agent_end","messages":[],"willRetry":true}))?;
        std::fs::write("accepted", "yes")?;
        if mode == "gated" {
            wait_for_release();
        }
        if !matches!(mode.as_str(), "cancel" | "stubborn" | "fast" | "flood") {
            finish_run(&mode)?;
        }
        if matches!(mode.as_str(), "flood" | "post_settle_flood") {
            for _ in 0..4096 {
                emit(&json!({"type":"tool_execution_update","data":"x".repeat(1024)}))?;
            }
            std::fs::write("flooded", "yes")?;
        }
    }
    for line in lines.by_ref() {
        let command: Value = serde_json::from_str(&line?)?;
        if command.get("type").and_then(Value::as_str) == Some("abort") {
            std::fs::write("aborted", "yes")?;
            if mode != "stubborn" {
                emit(&json!({"type":"response","id":"abort","command":"abort","success":true}))?;
                break;
            }
        }
    }
    drop(lines);
    if mode == "stubborn" {
        loop {
            std::thread::park();
        }
    }
    Ok(())
}

fn finish_run(mode: &str) -> io::Result<()> {
    match mode {
        "failure" => emit(&assistant("SWITCHBOARD_FAILED: missing capability", "stop"))?,
        "provider_error" => emit(&assistant("", "error"))?,
        "aborted_result" => emit(&assistant("", "aborted"))?,
        "long" => emit(&assistant(&"🙂".repeat(10000), "stop"))?,
        "retry" => {
            emit(&assistant("transient failure", "error"))?;
            emit(&json!({"type":"auto_retry_start","attempt":1}))?;
            emit(&json!({"type":"auto_retry_end","success":true}))?;
            emit(&assistant("Finished after retry", "stop"))?;
        }
        _ => emit(&assistant(
            "Finished task\nUnicode separator: \u{2028}",
            "stop",
        ))?,
    }
    emit(&json!({"type":"agent_settled"}))
}
