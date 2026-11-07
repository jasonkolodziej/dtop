import Testing
@testable import DtopCore

@Suite("Log stream decoding")
struct LogStreamTests {
    func frame(_ stream: UInt8, _ text: String) -> [UInt8] {
        let payload = Array(text.utf8)
        let size = UInt32(payload.count)
        return [stream, 0, 0, 0, UInt8(size >> 24), UInt8((size >> 16) & 0xFF), UInt8((size >> 8) & 0xFF), UInt8(size & 0xFF)] + payload
    }

    @Test("demultiplexes stdout and stderr frames")
    func multiplexed() {
        var decoder = LogStreamDecoder()
        let lines = decoder.feed(frame(1, "out 1\nout 2\n") + frame(2, "err\n"))
        #expect(lines == [
            LogLine(stream: .stdout, text: "out 1"), LogLine(stream: .stdout, text: "out 2"), LogLine(stream: .stderr, text: "err")
        ])
        #expect(decoder.finish().isEmpty)
    }

    @Test("frames split across feeds are reassembled")
    func splitFrames() {
        let bytes = frame(1, "hello world\n") + frame(2, "x\n")
        for split in 1..<bytes.count {
            var decoder = LogStreamDecoder()
            let lines = decoder.feed(Array(bytes[..<split])) + decoder.feed(Array(bytes[split...])) + decoder.finish()
            #expect(lines.map(\.text) == ["hello world", "x"])
        }
    }

    @Test("a line spanning multiple frames is joined per stream")
    func lineAcrossFrames() {
        var decoder = LogStreamDecoder()
        let lines = decoder.feed(frame(1, "par") + frame(2, "E\n") + frame(1, "tial\n"))
        #expect(lines == [LogLine(stream: .stderr, text: "E"), LogLine(stream: .stdout, text: "partial")])
    }

    @Test("TTY containers send raw text")
    func raw() {
        var decoder = LogStreamDecoder()
        let lines = decoder.feed(Array("plain line\r\nsecond".utf8)) + decoder.finish()
        #expect(lines == [LogLine(stream: .stdout, text: "plain line"), LogLine(stream: .stdout, text: "second")])
    }

    @Test("mode detection waits for a full header before deciding")
    func detection() {
        #expect(LogStreamDecoder.detectMode([]) == nil)
        #expect(LogStreamDecoder.detectMode([1]) == nil)
        #expect(LogStreamDecoder.detectMode([1, 0, 0]) == nil)
        #expect(LogStreamDecoder.detectMode([1, 0, 0, 0]) == .multiplexed)
        #expect(LogStreamDecoder.detectMode([1, 65]) == .raw)
        #expect(LogStreamDecoder.detectMode([72, 105]) == .raw)
        #expect(LogStreamDecoder.detectMode([2, 0, 0, 7]) == .raw)
    }

    @Test("a tiny raw output that looks like a header prefix is flushed at finish")
    func tinyRaw() {
        var decoder = LogStreamDecoder()
        #expect(decoder.feed([1]).isEmpty)
        #expect(decoder.finish() == [LogLine(stream: .stdout, text: "")])
    }

    @Test("forced modes skip detection")
    func forcedModes() {
        var raw = LogStreamDecoder(mode: .raw)
        #expect(raw.feed(frame(1, "x\n")).count == 1)
        var multiplexed = LogStreamDecoder(mode: .multiplexed)
        #expect(multiplexed.feed(Array("abc".utf8)).isEmpty)
    }

    @Test("terminal escapes, control characters and CRs are stripped; tabs expand")
    func cleaning() {
        #expect(LogStreamDecoder.clean(Array("\u{1B}[31mred\u{1B}[0m text".utf8)) == "red text")
        #expect(LogStreamDecoder.clean(Array("\u{1B}]0;title\u{07}after".utf8)) == "after")
        #expect(LogStreamDecoder.clean(Array("\u{1B}]8;;http://x\u{1B}\\link".utf8)) == "link")
        #expect(LogStreamDecoder.clean(Array("a\tb\r".utf8)) == "a    b")
        #expect(LogStreamDecoder.clean([0x07, 0x41, 0x7F, 0x42]) == "AB")
        #expect(LogStreamDecoder.clean(Array("héllo ✓".utf8)) == "héllo ✓")
    }
}

@Suite("Docker timestamps")
struct TimestampTests {
    @Test("parses UTC timestamps with nanosecond precision")
    func nanoseconds() throws {
        let value = try #require(DockerTimestamp.parse("2024-05-01T12:34:56.123456789Z"))
        #expect(abs(value - 1_714_566_896.123456789) < 1e-6)
    }

    @Test("parses well-known instants", arguments: [
        ("1970-01-01T00:00:00Z", 0.0),
        ("2000-02-29T00:00:00Z", 951_782_400.0),
        ("2024-01-01T00:00:00.5Z", 1_704_067_200.5),
        ("2024-05-01T14:34:56+02:00", 1_714_566_896.0),
        ("2024-05-01T07:04:56-05:30", 1_714_566_896.0),
        ("2024-05-01 12:34:56Z", 1_714_566_896.0)
    ])
    func instants(text: String, expected: Double) throws {
        let value = try #require(DockerTimestamp.parse(text))
        #expect(abs(value - expected) < 1e-6)
    }

    @Test("rejects zero time and malformed input", arguments: [
        "0001-01-01T00:00:00Z", "", "2024-05-01", "2024-13-01T00:00:00Z", "2024-05-01T25:00:00Z",
        "2024-05-01T12:34:56", "2024-05-01T12:34:56Q", "abcd-ef-ghTij:kl:mnZ", "2024-05-01T12:34:56+0200"
    ])
    func invalid(text: String) {
        #expect(DockerTimestamp.parse(text) == nil)
    }

    @Test("civil day computation agrees at era boundaries")
    func civil() {
        #expect(DockerTimestamp.daysFromCivil(year: 1970, month: 1, day: 1) == 0)
        #expect(DockerTimestamp.daysFromCivil(year: 1969, month: 12, day: 31) == -1)
        #expect(DockerTimestamp.daysFromCivil(year: 2000, month: 3, day: 1) == 11_017)
        #expect(DockerTimestamp.daysFromCivil(year: 2400, month: 1, day: 1) == 157_054)
    }
}
