import Testing
@testable import DtopCore

@Suite("Compose grouping")
struct ComposeGroupingTests {
    let containers = [
        Fixtures.container("shop-web-2", project: "shop", service: "web", number: 2, workingDir: "/srv/shop"),
        Fixtures.container("shop-web-1", project: "shop", service: "web", number: 1, workingDir: "/srv/shop"),
        Fixtures.container("shop-db-1", state: .exited, project: "shop", service: "db", number: 1, workingDir: "/old/shop"),
        Fixtures.container("blog-app-1", project: "blog", service: "app", workingDir: "/srv/blog"),
        Fixtures.container("zeta"),
        Fixtures.container("alpha", state: .exited)
    ]

    @Test("groups by project and service with stable ordering")
    func grouping() {
        let (projects, standalone) = ComposeProject.group(containers)
        #expect(projects.map(\.name) == ["blog", "shop"])
        #expect(standalone.map(\.name) == ["alpha", "zeta"])
        let shop = projects[1]
        #expect(shop.services.map(\.name) == ["db", "web"])
        #expect(shop.services[1].containers.map(\.name) == ["shop-web-1", "shop-web-2"])
        #expect(shop.containers.count == 3)
        #expect(shop.runningCount == 2)
        #expect(shop.services[1].id == "shop/web")
        #expect(shop.id == "shop")
    }

    @Test("working directory is the majority value")
    func workingDirectory() {
        let shop = ComposeProject.group(containers).projects[1]
        #expect(shop.workingDirectory == "/srv/shop")
    }

    @Test("containers without a service label land in an unknown service")
    func unknownService() {
        let (projects, _) = ComposeProject.group([Fixtures.container("x", project: "p")])
        #expect(projects[0].services.map(\.name) == ["(unknown)"])
    }

    @Test("empty project labels are treated as standalone")
    func emptyProject() {
        let (projects, standalone) = ComposeProject.group([Fixtures.container("x", project: "")])
        #expect(projects.isEmpty)
        #expect(standalone.count == 1)
    }

    @Test("aggregate status reflects the worst replica", arguments: [
        ([ContainerState.running, .running], ComposeStatus.running),
        ([.running, .exited], .partial),
        ([.exited, .created], .stopped),
        ([.paused, .paused], .paused),
        ([.paused, .exited], .partial),
        ([.running, .restarting], .degraded),
        ([.running, .dead], .degraded),
        ([], .empty)
    ])
    func status(states: [ContainerState], expected: ComposeStatus) {
        let members = states.enumerated().map { Fixtures.container("c\($0.offset)", state: $0.element) }
        #expect(ComposeStatus.aggregate(members) == expected)
    }

    @Test("an unhealthy container degrades the project")
    func unhealthy() {
        let members = [Fixtures.container("a", status: "Up 1 minute (unhealthy)")]
        #expect(ComposeStatus.aggregate(members) == .degraded)
    }
}

@Suite("Compose operations")
struct ComposeOperationTests {
    @Test("engine-native operations map to container actions")
    func engineNative() {
        #expect(ComposeOperation.start.containerAction() == .start)
        #expect(ComposeOperation.stop.containerAction(stopTimeout: 3) == .stop(timeoutSeconds: 3))
        #expect(ComposeOperation.restart.containerAction() == .restart(timeoutSeconds: nil))
        #expect(ComposeOperation.pause.containerAction() == .pause)
        #expect(ComposeOperation.unpause.containerAction() == .unpause)
        for operation in [ComposeOperation.up, .down, .pull] {
            #expect(operation.containerAction() == nil)
            #expect(!operation.isEngineNative)
        }
        #expect(ComposeOperation.down.isDestructive)
        #expect(!ComposeOperation.stop.isDestructive)
    }

    @Test("operations only target containers in a compatible state")
    func applicability() {
        let running = Fixtures.container("r")
        let exited = Fixtures.container("e", state: .exited)
        let paused = Fixtures.container("p", state: .paused)
        #expect(ComposeOperation.start.applies(to: exited))
        #expect(!ComposeOperation.start.applies(to: running))
        #expect(ComposeOperation.stop.applies(to: running))
        #expect(ComposeOperation.stop.applies(to: paused))
        #expect(!ComposeOperation.stop.applies(to: exited))
        #expect(ComposeOperation.pause.applies(to: running))
        #expect(!ComposeOperation.pause.applies(to: paused))
        #expect(ComposeOperation.unpause.applies(to: paused))
        #expect(ComposeOperation.restart.applies(to: exited))
        #expect(!ComposeOperation.up.applies(to: exited))
    }

    @Test("CLI invocations carry project name, directory and files")
    func invocation() {
        let project = ComposeProject(name: "shop", workingDirectory: "/srv/shop",
                                     configFiles: ["/srv/shop/compose.yaml", "/srv/shop/prod.yaml"], services: [])
        let up = ComposeCommandBuilder.invocation(for: .up, project: project)
        #expect(up.executable == "docker")
        #expect(up.arguments == ["compose", "--project-name", "shop", "--project-directory", "/srv/shop",
                                 "--file", "/srv/shop/compose.yaml", "--file", "/srv/shop/prod.yaml", "up", "--detach"])
        #expect(up.workingDirectory == "/srv/shop")
        let down = ComposeCommandBuilder.invocation(for: .down, project: project, endpoint: .tcp(host: "h", port: 1))
        #expect(down.arguments.prefix(2) == ["--host", "tcp://h:1"])
        #expect(down.arguments.suffix(2) == ["down", "--remove-orphans"])
        let pull = ComposeCommandBuilder.invocation(for: .pull, project: ComposeProject(name: "bare", services: []),
                                                    dockerExecutable: "/usr/bin/docker")
        #expect(pull.executable == "/usr/bin/docker")
        #expect(pull.arguments == ["compose", "--project-name", "bare", "pull"])
        #expect(pull.workingDirectory == nil)
    }

    @Test("display strings are shell-quoted")
    func quoting() {
        let invocation = CommandInvocation(executable: "docker", arguments: ["compose", "-f", "my file.yaml", "it's", ""])
        #expect(invocation.displayString == #"docker compose -f 'my file.yaml' 'it'\''s' ''"#)
        #expect(CommandInvocation.quote("/a/b-c_d.e:f=g@h,i+j") == "/a/b-c_d.e:f=g@h,i+j")
    }

    @Test("executables resolve through PATH with .exe on Windows")
    func pathResolution() {
        let posix = ProcessCommandRunner.resolve("docker", environment: ["PATH": "/usr/local/bin:/usr/bin"], platform: .linux,
                                                 fileExists: { $0 == "/usr/bin/docker" })
        #expect(posix == "/usr/bin/docker")
        let windows = ProcessCommandRunner.resolve("docker", environment: ["Path": #"C:\Tools;C:\Docker\bin"#], platform: .windows,
                                                   fileExists: { $0 == #"C:\Docker\bin\docker.exe"# })
        #expect(windows == #"C:\Docker\bin\docker.exe"#)
        #expect(ProcessCommandRunner.resolve("docker", environment: [:], platform: .linux, fileExists: { _ in true }) == nil)
        #expect(ProcessCommandRunner.resolve("/opt/docker", environment: [:], platform: .macOS, fileExists: { _ in true })
            == "/opt/docker")
        #expect(ProcessCommandRunner.resolve("/opt/docker", environment: [:], platform: .macOS, fileExists: { _ in false }) == nil)
    }

    @Test("a missing executable produces an actionable error")
    func missingExecutable() async {
        let runner = ProcessCommandRunner(environment: ["PATH": ""], platform: .current)
        await #expect(throws: DockerError.self) {
            _ = try await runner.run(CommandInvocation(executable: "definitely-not-a-real-dtop-binary", arguments: []))
        }
    }

    #if !os(Windows)
    @Test("runs a real process and captures output and exit status")
    func realProcess() async throws {
        let runner = ProcessCommandRunner()
        let ok = try await runner.run(CommandInvocation(executable: "sh", arguments: ["-c", "echo hi; echo err >&2"]))
        #expect(ok.succeeded)
        #expect(ok.output.contains("hi"))
        #expect(ok.output.contains("err"))
        let failed = try await runner.run(CommandInvocation(executable: "sh", arguments: ["-c", "exit 3"]))
        #expect(failed.exitCode == 3)
        #expect(!failed.succeeded)
    }
    #endif
}
