import Testing
@testable import DtopApp
@testable import DtopCore
@testable import DtopUI

@Suite("Brand mark")
struct BrandTests {
    func render(_ snapshot: DockerSnapshot, theme: Theme = .harbor, columns: Int = 120, rows: Int = 40) -> RenderedFrame {
        AppView.render(RenderContext(state: AppState(), snapshot: snapshot, theme: theme,
                                     size: TerminalSize(columns: columns, rows: rows), now: 1_000))
    }

    @Test("the header starts with the mark, colored by the theme's roles")
    func header() {
        for theme in [Theme.harbor, Theme.daylight] {
            let frame = render(AppFixtures.snapshot(), theme: theme)
            #expect(frame.buffer.text(row: 0).hasPrefix(" > ▃▅▇ dtop "))
            let expected: [(Int, ThemeRole)] = [(1, .accent), (3, .success), (4, .memory), (5, .gpu)]
            for (column, role) in expected {
                #expect(frame.buffer[column, 0].style.foreground == theme[role], "\(theme.name) column \(column)")
            }
            #expect(frame.buffer[1, 0].style.attributes.contains(.bold), "the chevron is bold")
            #expect(frame.buffer[7, 0].style.foreground == theme[.title], "the name follows the mark")
            #expect(frame.buffer[7, 0].style.background == theme[.headerBackground])
        }
    }

    @Test("every row of the large mark has the same width")
    func largeRows() {
        for row in BrandMark.large {
            #expect(TextWidth.of(row.map(\.text).joined()) == BrandMark.largeWidth)
        }
    }

    @Test("connecting and offline screens show the large mark and version", arguments: [
        ConnectionState.connecting,
        .disconnected(message: "Cannot connect to Docker", retryAt: 1_004)
    ])
    func splash(connection: ConnectionState) {
        let frame = render(DockerSnapshot(endpoint: "unix:///x.sock", connection: connection))
        let lines = frame.buffer.lines
        guard let top = lines.firstIndex(where: { $0.contains("▀█▄          ██") }) else {
            Issue.record("large mark not drawn")
            return
        }
        let first = BrandMark.large.map { $0.map(\.text).joined() }
        for (offset, row) in first.enumerated() {
            #expect(lines[top + offset].contains(row))
        }
        #expect(lines[top + BrandMark.large.count + 1].contains("dtop \(AppProfile.current.version)"))
        let column = lines[top].distance(from: lines[top].startIndex, to: lines[top].range(of: "▀█▄")!.lowerBound)
        #expect(frame.buffer[column, top].style.foreground == Theme.harbor[.accent])
        let status = frame.buffer.plainText
        #expect(status.contains("Connecting to Docker") || status.contains("Retrying in 4s"))
    }

    @Test("without room, or once connected, the large mark is left out")
    func noSplash() {
        let small = render(DockerSnapshot(endpoint: "unix:///x.sock", connection: .connecting), columns: 60, rows: 12)
        #expect(!small.buffer.plainText.contains("▀█▄"))
        #expect(small.buffer.plainText.contains("Connecting to Docker"))
        let connected = render(DockerSnapshot(endpoint: "unix:///x.sock", connection: .connected))
        #expect(!connected.buffer.plainText.contains("▀█▄"))
        #expect(connected.buffer.plainText.contains("No containers yet"))
    }

    @Test("the header mark and splash never overflow tiny terminals")
    func tiny() {
        for (columns, rows) in [(1, 1), (3, 2), (8, 4), (20, 10), (40, 14)] {
            for connection in [ConnectionState.connecting, .connected] {
                let frame = render(DockerSnapshot(connection: connection), columns: columns, rows: rows)
                #expect(frame.buffer.width == columns)
            }
        }
    }
}
