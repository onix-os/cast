async fn clone(url: &Url, path: &Path, pb: &ProgressBar) -> Result<gitwrap::Repository, gitwrap::Error> {
    let (progress, reporter) = progress_reporter(pb);

    let result = gitwrap::Repository::clone_mirror_progress_with_limits(path, url, MASON_GIT_LIMITS, progress).await;
    let _ = reporter.await;
    pb.finish_and_clear();

    result
}

async fn fetch(repo: &gitwrap::Repository, pb: &ProgressBar) -> Result<(), gitwrap::Error> {
    let (progress, reporter) = progress_reporter(pb);

    let result = repo.fetch_progress(progress).await;
    let _ = reporter.await;
    pb.finish_and_clear();

    result
}

fn progress_reporter(pb: &ProgressBar) -> (mpsc::Sender<gitwrap::FetchProgress>, tokio::task::JoinHandle<()>) {
    pb.set_length(100);
    pb.set_style(
        ProgressStyle::with_template(" {spinner} |{percent:>3}%| {wide_msg} {prefix:>.dim} ")
            .unwrap()
            .tick_chars("--=≡■≡=--"),
    );
    let (sender, mut receiver) = mpsc::channel::<gitwrap::FetchProgress>(64);
    let pb = pb.clone();
    let reporter = tokio::spawn(async move {
        while let Some(progress) = receiver.recv().await {
            pb.set_position(u64::from(progress.percent));
            pb.set_prefix(progress.speed);
        }
    });
    (sender, reporter)
}
