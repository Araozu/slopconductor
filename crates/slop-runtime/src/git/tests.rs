use std::path::PathBuf;

use tokio::sync::watch;

use super::*;

#[tokio::test]
async fn managed_worktree_supports_canonical_roots_and_long_tracked_paths() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("repository with spaces");
    std::fs::create_dir(&source).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "core.longpaths=true",
            ])
            .args(args)
            .current_dir(&source)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    // Runtime Git configuration must override a repository's short-path
    // default, without changing the user's persisted configuration.
    git(&["config", "core.longpaths", "false"]);
    let mut tracked = PathBuf::new();
    for index in 0..5 {
        tracked = tracked.join(format!("{index}-{}end", "deep path ".repeat(4)));
    }
    tracked = tracked.join("file.txt");
    std::fs::create_dir_all(source.join(tracked.parent().unwrap())).unwrap();
    std::fs::write(source.join(&tracked), "before\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "base"]);

    let data = directory.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let data = std::fs::canonicalize(data).unwrap();
    let service = GitService::new(data.clone());
    let repo = service.inspect(source.to_str().unwrap()).await.unwrap();
    let base = service.resolve(&repo, None).await.unwrap();
    let workspace = data.join("workspaces").join("project").join("workspace");
    let path = workspace.to_str().unwrap();
    assert!(workspace.join(&tracked).to_str().unwrap().len() > 260);
    let (_sender, mut cancel) = watch::channel(false);
    service
        .allocate(&repo, path, &base, &mut cancel)
        .await
        .unwrap();
    assert_eq!(std::fs::canonicalize(&workspace).unwrap(), workspace);
    assert_eq!(
        std::fs::read_to_string(workspace.join(&tracked)).unwrap(),
        "before\n"
    );
    std::fs::write(workspace.join(&tracked), "after\n").unwrap();
    let diff = service.diff(&repo, path, &base).await.unwrap();
    assert_eq!(diff.head_commit, base);
    assert!(diff.patch.contains("+after"));
    assert!(diff.patch.contains("-before"));
    let error = service
        .remove(&repo, path, &base, &mut cancel)
        .await
        .unwrap_err();
    assert_eq!(error.code, "workspace_dirty");
    assert!(!error.effects_unknown);
    assert!(workspace.is_dir());
    assert_eq!(
        std::fs::read_to_string(source.join(&tracked)).unwrap(),
        "before\n"
    );
    std::fs::write(workspace.join(&tracked), "before\n").unwrap();
    service
        .remove(&repo, path, &base, &mut cancel)
        .await
        .unwrap();
    assert!(!workspace.exists());
}
