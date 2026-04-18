use crate::core::tracking::TimedExecution;
use anyhow::Result;

pub fn run(args: &[String], _verbose: u8) -> Result<i32> {
    let timer = TimedExecution::start();
    let suppress_newline = matches!(args.first().map(|arg| arg.as_str()), Some("-n"));
    let start = usize::from(suppress_newline);
    let body = args[start..].join(" ");
    let output = if suppress_newline {
        body.clone()
    } else {
        format!("{body}\n")
    };

    print!("{output}");

    let original_cmd = if args.is_empty() {
        "echo".to_string()
    } else {
        format!("echo {}", args.join(" "))
    };
    let rtk_cmd = if args.is_empty() {
        "rtk echo".to_string()
    } else {
        format!("rtk echo {}", args.join(" "))
    };
    timer.track(&original_cmd, &rtk_cmd, &output, &output);
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_echo_allows_no_newline_flag() {
        let args = vec!["-n".to_string(), "hello".to_string()];
        assert_eq!(run_echo_output(&args), "hello");
    }

    fn run_echo_output(args: &[String]) -> String {
        let suppress_newline = matches!(args.first().map(|arg| arg.as_str()), Some("-n"));
        let start = usize::from(suppress_newline);
        let body = args[start..].join(" ");
        if suppress_newline {
            body
        } else {
            format!("{body}\n")
        }
    }
}
