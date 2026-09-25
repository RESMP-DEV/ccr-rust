use anyhow::Result;
use serde_json::Value;
use std::io::Write;

fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("unknown")
}

fn single_line(value: &Value) -> String {
    super::report::text(string(value), 16000).replace(['\n', '\t', '\r'], " ")
}

fn event(output: &mut impl Write, value: &Value) -> Result<()> {
    writeln!(
        output,
        "  @{} {} {} {}{}",
        value["cursor"],
        single_line(&value["event"]),
        single_line(&value["item_id"]),
        single_line(&value["summary"]["text"]),
        if value["summary"]["truncated"] == true {
            " …"
        } else {
            ""
        }
    )?;
    if let Some(exit_code) = value["exit_code"].as_i64() {
        writeln!(output, "    exit={exit_code}")?;
    }
    Ok(())
}

fn headline(output: &mut impl Write, run: &Value) -> Result<()> {
    writeln!(
        output,
        "{}  {} (unverified)  commands={} failed={} failed_items={} files={}  tokens={}/{}{}",
        single_line(&run["run_id"]),
        single_line(&run["outcome"]),
        run["commands_completed"],
        run["failed_commands"],
        run["failed_items"],
        run["file_changes"],
        run["usage"]["input_tokens"],
        run["usage"]["output_tokens"],
        if run["needs_attention"] == true {
            "  ATTENTION"
        } else {
            ""
        }
    )?;
    Ok(())
}

pub(super) fn write(output: &mut impl Write, value: &Value) -> Result<()> {
    if let Some(runs) = value["runs"].as_array() {
        for run in runs {
            headline(output, run)?;
        }
        writeln!(
            output,
            "{} runs shown; {} omitted. Use workers show RUN for evidence.",
            runs.len(),
            value["omitted_runs"]
        )?;
    } else if let Some(events) = value["events"].as_array() {
        for entry in events {
            event(output, entry)?;
        }
        writeln!(
            output,
            "next_cursor={} has_more={} partial_line={} skipped={}/{}",
            value["next_cursor"],
            value["has_more"],
            value["trace"]["partial_line"],
            value["trace"]["malformed_lines"],
            value["trace"]["oversized_lines"]
        )?;
    } else if value.get("detail").is_some() {
        event(output, value)?;
        writeln!(
            output,
            "{}{}",
            string(&value["detail"]["text"]),
            if value["detail"]["truncated"] == true {
                "\n[truncated; increase --max-chars, maximum 16000]"
            } else {
                ""
            }
        )?;
    } else {
        headline(output, value)?;
        writeln!(
            output,
            "Requested model: {} (provider not verified by saved trace)",
            single_line(&value["model_requested"])
        )?;
        writeln!(
            output,
            "Usage: input={} cached={} output={} reasoning={} reported_turns={}",
            value["usage"]["input_tokens"],
            value["usage"]["cached_input_tokens"],
            value["usage"]["output_tokens"],
            value["usage"]["reasoning_output_tokens"],
            value["usage"]["reported_turns"]
        )?;
        for (key, heading) in [
            ("active_items", "Started, no completion observed"),
            ("failures", "Failures (may be recovered)"),
            ("recent", "Recent activity"),
        ] {
            if let Some(entries) = value[key].as_array().filter(|entries| !entries.is_empty()) {
                writeln!(output, "{heading}:")?;
                for entry in entries {
                    event(output, entry)?;
                }
            }
        }
        if let Some(warnings) = value["warnings"].as_array() {
            for warning in warnings {
                writeln!(output, "Warning: {}", single_line(warning))?;
            }
        }
        if !value["final_report"].is_null() {
            writeln!(
                output,
                "Worker's final report (claim, not verification):\n{}{}",
                string(&value["final_report"]["text"]),
                if value["final_report"]["truncated"] == true {
                    "\n[truncated]"
                } else {
                    ""
                }
            )?;
        }
        writeln!(
            output,
            "Trace: {} bytes; cursor={}; stderr={} bytes (not included)",
            value["trace"]["file_bytes"], value["trace"]["next_cursor"], value["stderr_bytes"]
        )?;
        writeln!(output, "Receipt pending does not prove liveness. Use workers detail RUN CURSOR for specific output.")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;
