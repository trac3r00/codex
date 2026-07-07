#![allow(clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadStatus;
use codex_core::config::set_project_trust_level;
use codex_protocol::config_types::TrustLevel;
use divan::Bencher;
use tokio::runtime::Runtime;

const EXEC_SERVER_DELAY: Duration = Duration::from_millis(15);

fn main() {
    divan::main();
}

// The largest case takes tens of seconds with the fixed delay, so five samples
// per skill count keep the Bazel target usable.
#[divan::bench(args = [1, 8, 64], sample_count = 5, sample_size = 1)]
fn cold_skill_load(bencher: Bencher, skill_count: usize) {
    let runtime = Arc::new(Runtime::new().expect("benchmark Tokio runtime should start"));
    let setup_runtime = Arc::clone(&runtime);
    let bench_runtime = Arc::clone(&runtime);

    bencher
        .with_inputs(move || setup_runtime.block_on(ColdThreadStartCase::new(skill_count)))
        .bench_local_values(move |case| bench_runtime.block_on(case.run()));
}

struct ColdThreadStartCase {
    app_server: TestAppServer,
    project_root: String,
}

impl ColdThreadStartCase {
    async fn new(skill_count: usize) -> Self {
        let exec_server = codex_utils_cargo_bin::cargo_bin("exec-server")
            .expect("benchmark exec-server fixture should be available");
        let mut app_server = TestAppServer::builder()
            .without_debug_only_test_args()
            .with_exec_server_delay(EXEC_SERVER_DELAY)
            .with_exec_server_program(&exec_server)
            .build()
            .await
            .expect("benchmark app-server fixture should start");
        let codex_home = app_server.codex_home();
        // Emulate the part of git that config/project-root discovery needs: it
        // only checks for a .git marker. This repo does not provide a prebuilt
        // git binary to Bazel benchmarks, and using host git would make this
        // benchmark non-hermetic.
        std::fs::create_dir_all(codex_home.join(".git"))
            .expect("benchmark CODEX_HOME project marker should be created");
        // Optimized app-server binaries do not expose the debug-only startup
        // task switch used by functional tests. Disable plugins in this
        // skill-only benchmark instead so background catalog work stays out of
        // the measured thread start.
        std::fs::write(
            codex_home.join("config.toml"),
            "[features]\nplugins = false\n",
        )
        .expect("benchmark plugin configuration should be written");
        set_project_trust_level(codex_home, codex_home, TrustLevel::Trusted)
            .expect("benchmark CODEX_HOME should be trusted");
        // Fixed-delay interposition is host-local today. Seed the selected
        // project root directly so corpus construction is outside the timed
        // thread/start request.
        let auto_env = app_server
            .auto_env_params()
            .expect("benchmark auto environment should exist");
        let project_root = auto_env
            .cwd
            .to_inferred_abs_path()
            .expect("fixed-delay benchmark requires a host-native project root");
        // Give the synthetic workspace its own project boundary so config
        // discovery does not inherit unrelated parent folders such as
        // /tmp/.codex.
        std::fs::create_dir_all(project_root.join(".git"))
            .expect("benchmark project marker should be created");
        seed_short_project_skills(project_root.as_path(), skill_count)
            .expect("benchmark skills should be seeded");
        set_project_trust_level(codex_home, project_root.as_path(), TrustLevel::Trusted)
            .expect("benchmark project should be trusted");
        app_server
            .initialize()
            .await
            .expect("benchmark app-server should initialize");
        Self {
            app_server,
            project_root: auto_env.cwd.as_str().to_string(),
        }
    }

    async fn run(mut self) {
        let request_id = self
            .app_server
            .send_thread_start_request_with_auto_env(ThreadStartParams {
                cwd: Some(self.project_root),
                ..Default::default()
            })
            .await
            .expect("thread/start request should be sent");
        let response: JSONRPCResponse = self
            .app_server
            .read_stream_until_response_message(RequestId::Integer(request_id))
            .await
            .expect("thread/start response should arrive");
        let response: ThreadStartResponse =
            to_response(response).expect("thread/start response should be successful");
        assert_eq!(response.thread.status, ThreadStatus::Idle);
    }
}

fn seed_short_project_skills(project_root: &Path, skill_count: usize) -> std::io::Result<()> {
    let skills_root = project_root.join(".agents/skills");
    for index in 0..skill_count {
        let name = format!("benchmark-skill-{index:02}");
        let skill_directory = skills_root.join(&name);
        std::fs::create_dir_all(&skill_directory)?;
        let contents =
            format!("---\nname: {name}\ndescription: short benchmark skill {index}\n---\n");
        std::fs::write(skill_directory.join("SKILL.md"), contents)?;
    }
    Ok(())
}
