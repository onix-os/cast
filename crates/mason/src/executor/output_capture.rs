#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

impl std::fmt::Display for OutputStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

#[derive(Debug, Default)]
struct OutputBudget {
    total: u64,
}

#[derive(Debug)]
struct OutputAdmission {
    accepted: usize,
    violation: Option<StepExecutionError>,
}

impl OutputBudget {
    fn admit(
        &mut self,
        stream: OutputStream,
        stream_bytes: &mut u64,
        bytes: usize,
        limits: StepExecutionLimits,
    ) -> OutputAdmission {
        let bytes = u64::try_from(bytes).expect("log read buffer length fits in u64");
        let stream_limit = limits.stream_limit(stream);
        let stream_remaining = stream_limit.saturating_sub(*stream_bytes);
        let total_remaining = limits.total_output_bytes.saturating_sub(self.total);
        let accepted = bytes.min(stream_remaining).min(total_remaining);
        *stream_bytes += accepted;
        self.total += accepted;

        let violation = if accepted == bytes {
            None
        } else if stream_remaining <= total_remaining {
            Some(StepExecutionError::OutputLimit {
                stream,
                limit: stream_limit,
                observed: stream_limit.saturating_add(1),
            })
        } else {
            Some(StepExecutionError::TotalOutputLimit {
                limit: limits.total_output_bytes,
                observed: limits.total_output_bytes.saturating_add(1),
            })
        };

        OutputAdmission {
            accepted: usize::try_from(accepted).expect("accepted bytes came from a usize-sized read"),
            violation,
        }
    }
}

#[derive(Debug)]
struct LogMux {
    mode: LogMode,
    current: Option<OutputStream>,
    at_line_start: bool,
}

impl LogMux {
    const fn new(mode: LogMode) -> Self {
        Self {
            mode,
            current: None,
            at_line_start: true,
        }
    }

    fn emit(&mut self, stream: OutputStream, mut bytes: &[u8]) -> io::Result<()> {
        if self.mode == LogMode::Discard || bytes.is_empty() {
            return Ok(());
        }

        let stdout = io::stdout();
        let mut output = stdout.lock();
        if self.current != Some(stream) && !self.at_line_start {
            output.write_all(b"\n")?;
            self.at_line_start = true;
        }
        self.current = Some(stream);

        while !bytes.is_empty() {
            if self.at_line_start {
                write!(output, "{} ", "│".dim())?;
                self.at_line_start = false;
            }

            let segment_len = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |newline| newline + 1);
            let (segment, remaining) = bytes.split_at(segment_len);
            output.write_all(segment)?;
            if segment.last() == Some(&b'\n') {
                self.at_line_start = true;
            }
            bytes = remaining;
        }
        output.flush()
    }

    fn finish(&mut self, stream: OutputStream) -> io::Result<()> {
        if self.mode == LogMode::Discard || self.current != Some(stream) || self.at_line_start {
            return Ok(());
        }

        let stdout = io::stdout();
        let mut output = stdout.lock();
        output.write_all(b"\n")?;
        output.flush()?;
        self.at_line_start = true;
        Ok(())
    }
}

type LogReader = thread::JoinHandle<Result<(), StepExecutionError>>;

#[allow(clippy::too_many_arguments)]
fn spawn_log_reader<R>(
    pipe: R,
    stream: OutputStream,
    limits: StepExecutionLimits,
    output_budget: Arc<Mutex<OutputBudget>>,
    log_mux: Arc<Mutex<LogMux>>,
    stop: Arc<AtomicBool>,
    alert: mpsc::Sender<()>,
) -> io::Result<LogReader>
where
    R: io::Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("mason-step-{stream}"))
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drain_log(pipe, stream, limits, &output_budget, &log_mux, &stop)
            }));
            match result {
                Ok(result) => {
                    if result.is_err() {
                        let _ = alert.send(());
                    }
                    result
                }
                Err(payload) => {
                    let _ = alert.send(());
                    std::panic::resume_unwind(payload)
                }
            }
        })
}

fn drain_log<R>(
    mut pipe: R,
    stream: OutputStream,
    limits: StepExecutionLimits,
    output_budget: &Mutex<OutputBudget>,
    log_mux: &Mutex<LogMux>,
    stop: &AtomicBool,
) -> Result<(), StepExecutionError>
where
    R: io::Read,
{
    let mut buffer = [0_u8; LOG_READ_BUFFER_BYTES];
    let mut stream_bytes = 0_u64;

    loop {
        let read = match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(STEP_MONITOR_INTERVAL);
                continue;
            }
            Err(source) => return Err(StepExecutionError::OutputRead { stream, source }),
        };

        let admission = output_budget
            .lock()
            .map_err(|_| StepExecutionError::OutputBudgetPoisoned)?
            .admit(stream, &mut stream_bytes, read, limits);

        if admission.accepted > 0 {
            log_mux
                .lock()
                .map_err(|_| StepExecutionError::LogMuxPoisoned)?
                .emit(stream, &buffer[..admission.accepted])
                .map_err(|source| StepExecutionError::OutputWrite { stream, source })?;
        }

        if let Some(violation) = admission.violation {
            return Err(violation);
        }
    }

    log_mux
        .lock()
        .map_err(|_| StepExecutionError::LogMuxPoisoned)?
        .finish(stream)
        .map_err(|source| StepExecutionError::OutputWrite { stream, source })
}

fn join_log_reader(reader: &mut Option<LogReader>, stream: OutputStream) -> Result<(), StepExecutionError> {
    let Some(reader) = reader.take() else {
        return Ok(());
    };
    match reader.join() {
        Ok(result) => result,
        Err(_) => Err(StepExecutionError::ReaderThreadPanicked { stream }),
    }
}

fn target_prefix(target: &str, index: usize) -> String {
    format!("{}{}", if index > 0 { "\n" } else { "" }, target.dim())
}

fn pgo_stage_prefix(stage: &str, index: usize) -> String {
    let newline = if index > 0 {
        format!("{}\n", "│".dim())
    } else {
        String::new()
    };
    format!("{newline}{}", format!("│pgo-{stage}").dim())
}

fn phase_prefix(phase: &str, is_pgo: bool, index: usize) -> String {
    let pipes = if is_pgo { "││".dim() } else { "│".dim() };
    let newline = if index > 0 { format!("{pipes}\n") } else { String::new() };
    format!("{newline}{pipes}{}", phase.dim())
}

fn parse_pgo_stage(stage: &str) -> Result<Stage, Error> {
    match stage {
        "one" => Ok(Stage::One),
        "two" => Ok(Stage::Two),
        "use" => Ok(Stage::Use),
        _ => Err(Error::UnsupportedPgoStage(stage.to_owned())),
    }
}

fn parse_phase(phase: &str) -> Result<Phase, Error> {
    match phase.to_ascii_lowercase().as_str() {
        "prepare" => Ok(Phase::Prepare),
        "setup" => Ok(Phase::Setup),
        "build" => Ok(Phase::Build),
        "install" => Ok(Phase::Install),
        "check" => Ok(Phase::Check),
        "workload" => Ok(Phase::Workload),
        _ => Err(Error::UnsupportedPhase(phase.to_owned())),
    }
}
