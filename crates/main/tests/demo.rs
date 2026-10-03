#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert behavior while propagating process errors"
)]
use std::process::Command;
use switchboard_uuids::PeerId;

#[test]
fn demo_binary_prints_a_human_message_and_one_agent_reply() -> Result<(), Box<dyn std::error::Error>>
{
    let output = Command::new(env!("CARGO_BIN_EXE_switchboard"))
        .args(["demo", "--message", "hello from CLI"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    let human = lines.first().ok_or("missing human line")?;
    let agent = lines.last().ok_or("missing agent line")?;
    assert!(human.starts_with("Human ["));
    assert!(human.ends_with(": hello from CLI"));
    assert!(agent.starts_with("Agent ["));
    assert!(agent.ends_with(": hello from CLI"));
    let extract_id = |line: &str| -> Result<PeerId, Box<dyn std::error::Error>> {
        let (_, rest) = line.split_once('[').ok_or("missing ID")?;
        let (id, _) = rest.split_once(']').ok_or("missing closing bracket")?;
        Ok(id.parse()?)
    };
    let human_id = extract_id(human)?;
    let agent_id = extract_id(agent)?;
    assert_ne!(human_id, agent_id);
    assert!(agent.contains(&format!("Fake reply from {agent_id}:")));
    Ok(())
}

#[test]
fn demo_binary_rejects_blank_messages_without_printing_a_transcript() -> Result<(), std::io::Error>
{
    let output = Command::new(env!("CARGO_BIN_EXE_switchboard"))
        .args(["demo", "--message", " \n\t"])
        .output()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("message text must not be blank"));
    Ok(())
}
