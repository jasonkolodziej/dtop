import Testing
@testable import DtopUI

@Suite("Input parser: keys")
struct InputParserKeyTests {
    func parse(_ text: String) -> [InputEvent] {
        var parser = InputParser()
        return parser.feed(Array(text.utf8))
    }

    func key(_ key: Key, _ modifiers: KeyModifiers = []) -> InputEvent {
        .key(KeyEvent(key, modifiers: modifiers))
    }

    @Test("printable ASCII and UTF-8 characters")
    func printable() {
        #expect(parse("aZ9 ") == [key(.character("a")), key(.character("Z")), key(.character("9")), key(.character(" "))])
        #expect(parse("é中😀") == [key(.character("é")), key(.character("中")), key(.character("😀"))])
    }

    @Test("control bytes map to named keys and Ctrl+letter", arguments: [
        ([0x0D], Key.enter, KeyModifiers()), ([0x0A], .enter, []), ([0x09], .tab, []), ([0x7F], .backspace, []),
        ([0x08], .backspace, []), ([0x03], .character("c"), .ctrl), ([0x01], .character("a"), .ctrl),
        ([0x1A], .character("z"), .ctrl), ([0x00], .character(" "), .ctrl), ([0x1C], .character("\\"), .ctrl),
        ([0x1D], .character("]"), .ctrl), ([0x1E], .character("^"), .ctrl), ([0x1F], .character("_"), .ctrl)
    ] as [([UInt8], Key, KeyModifiers)])
    func controls(bytes: [UInt8], expected: Key, modifiers: KeyModifiers) {
        var parser = InputParser()
        #expect(parser.feed(bytes) == [key(expected, modifiers)])
    }

    @Test("CSI and SS3 navigation keys", arguments: [
        ("\u{1B}[A", Key.up), ("\u{1B}[B", .down), ("\u{1B}[C", .right), ("\u{1B}[D", .left),
        ("\u{1B}[H", .home), ("\u{1B}[F", .end), ("\u{1B}OA", .up), ("\u{1B}OH", .home), ("\u{1B}OF", .end),
        ("\u{1B}[1~", .home), ("\u{1B}[7~", .home), ("\u{1B}[4~", .end), ("\u{1B}[8~", .end),
        ("\u{1B}[2~", .insert), ("\u{1B}[3~", .delete), ("\u{1B}[5~", .pageUp), ("\u{1B}[6~", .pageDown),
        ("\u{1B}OM", .enter)
    ])
    func navigation(sequence: String, expected: Key) {
        #expect(parse(sequence) == [key(expected)])
    }

    @Test("function keys in all common encodings", arguments: [
        ("\u{1B}OP", 1), ("\u{1B}OQ", 2), ("\u{1B}OR", 3), ("\u{1B}OS", 4), ("\u{1B}[11~", 1), ("\u{1B}[15~", 5),
        ("\u{1B}[17~", 6), ("\u{1B}[21~", 10), ("\u{1B}[23~", 11), ("\u{1B}[24~", 12), ("\u{1B}[1;2P", 1)
    ])
    func functionKeys(sequence: String, number: Int) {
        guard case .key(let event)? = parse(sequence).first else {
            Issue.record("no key for \(sequence.debugDescription)")
            return
        }
        #expect(event.key == .function(number))
    }

    @Test("xterm modifier parameters decode shift/alt/ctrl")
    func modifiers() {
        #expect(parse("\u{1B}[1;5A") == [key(.up, .ctrl)])
        #expect(parse("\u{1B}[1;2B") == [key(.down, .shift)])
        #expect(parse("\u{1B}[1;3C") == [key(.right, .alt)])
        #expect(parse("\u{1B}[1;8D") == [key(.left, [.shift, .alt, .ctrl])])
        #expect(parse("\u{1B}[5;5~") == [key(.pageUp, .ctrl)])
        #expect(parse("\u{1B}[Z") == [key(.backTab, .shift)])
    }

    @Test("Alt is encoded as an ESC prefix")
    func alt() {
        #expect(parse("\u{1B}x") == [key(.character("x"), .alt)])
        #expect(parse("\u{1B}é") == [key(.character("é"), .alt)])
        #expect(parse("\u{1B}\r") == [key(.enter, .alt)])
    }

    @Test("CSI u (kitty/fixterms) keys")
    func csiU() {
        #expect(parse("\u{1B}[97;5u") == [key(.character("a"), .ctrl)])
        #expect(parse("\u{1B}[13u") == [key(.enter)])
        #expect(parse("\u{1B}[27u") == [key(.escape)])
        #expect(parse("\u{1B}[9;2u") == [key(.tab, .shift)])
        #expect(parse("\u{1B}[127u") == [key(.backspace)])
    }

    @Test("double ESC yields an escape and then waits for the rest")
    func doubleEscape() {
        var parser = InputParser()
        #expect(parser.feed([0x1B, 0x1B]) == [key(.escape)])
        #expect(parser.hasPendingInput)
        #expect(parser.flush() == [key(.escape)])
    }

    @Test("focus events and unknown sequences")
    func misc() {
        #expect(parse("\u{1B}[I") == [.focus(true)])
        #expect(parse("\u{1B}[O") == [.focus(false)])
        #expect(parse("\u{1B}[99~a") == [key(.character("a"))])
        #expect(parse("\u{1B}[?1;2cx") == [key(.character("x"))])
        #expect(parse("\u{1B}OXb") == [key(.character("b"))])
    }
}

@Suite("Input parser: buffering")
struct InputParserBufferingTests {
    @Test("partial escape sequences wait for more bytes")
    func partial() {
        var parser = InputParser()
        #expect(parser.feed([0x1B]).isEmpty)
        #expect(parser.hasPendingInput)
        #expect(parser.feed(Array("[".utf8)).isEmpty)
        #expect(parser.feed(Array("1;5".utf8)).isEmpty)
        #expect(parser.feed(Array("A".utf8)) == [.key(KeyEvent(.up, modifiers: .ctrl))])
        #expect(!parser.hasPendingInput)
    }

    @Test("partial UTF-8 waits for the continuation bytes")
    func partialUTF8() {
        var parser = InputParser()
        let bytes = Array("中".utf8)
        #expect(parser.feed([bytes[0]]).isEmpty)
        #expect(parser.feed([bytes[1]]).isEmpty)
        #expect(parser.feed([bytes[2]]) == [.key(KeyEvent(.character("中")))])
    }

    @Test("invalid UTF-8 becomes replacement characters")
    func invalidUTF8() {
        var parser = InputParser()
        #expect(parser.feed([0xFF, 0x41]) == [.key(KeyEvent(.character("\u{FFFD}"))), .key(KeyEvent(.character("A")))])
        #expect(parser.feed([0xC3, 0x41]) == [.key(KeyEvent(.character("\u{FFFD}"))), .key(KeyEvent(.character("A")))])
    }

    @Test("flush resolves a lone ESC, a dangling Alt prefix and truncated bytes")
    func flush() {
        var parser = InputParser()
        _ = parser.feed([0x1B])
        #expect(parser.flush() == [.key(KeyEvent(.escape))])
        #expect(!parser.hasPendingInput)

        _ = parser.feed(Array("\u{1B}[".utf8))
        #expect(parser.flush() == [.key(KeyEvent(.character("["), modifiers: .alt))])

        _ = parser.feed([0xE4, 0xB8])
        #expect(parser.flush().isEmpty)
        #expect(!parser.hasPendingInput)
        #expect(parser.flush().isEmpty)
    }

    @Test("a mouse report split across reads completes normally")
    func splitMouseReport() {
        var parser = InputParser()
        #expect(parser.feed(Array("\u{1B}[<0;".utf8)).isEmpty)
        #expect(parser.hasIncompleteSequence)
        #expect(parser.flushDelay > 0.2, "half a sequence gets a longer grace period than a lone ESC")
        #expect(parser.feed(Array("3;16M".utf8)) == [.mouse(MouseEvent(button: .left, action: .press, x: 2, y: 15))])
        #expect(!parser.hasIncompleteSequence)
    }

    @Test("an abandoned half sequence is dropped on flush, never re-read as keystrokes")
    func abandonedSequence() {
        var parser = InputParser()
        _ = parser.feed(Array("\u{1B}[<0;3;1".utf8))
        #expect(parser.flush().isEmpty, "digits like 3 and 1 would otherwise switch tabs")
        #expect(!parser.hasPendingInput)
        #expect(parser.feed(Array("6Mq".utf8)) == [
            .key(KeyEvent(.character("6"))), .key(KeyEvent(.character("M"))), .key(KeyEvent(.character("q")))
        ], "bytes after the drop are ordinary input")

        _ = parser.feed(Array("\u{1B}O".utf8))
        #expect(parser.hasIncompleteSequence)
        #expect(parser.flush() == [.key(KeyEvent(.character("O"), modifiers: .alt))], "a bare introducer is still Alt+key")
    }

    @Test("a lone ESC keeps the short timeout")
    func loneEscapeDelay() {
        var parser = InputParser()
        _ = parser.feed([0x1B])
        #expect(!parser.hasIncompleteSequence)
        #expect(parser.flushDelay < 0.1)
        var paste = InputParser()
        _ = paste.feed(Array("\u{1B}[200~partial".utf8))
        #expect(paste.hasIncompleteSequence, "an unfinished paste waits longer too")
    }

    @Test("an overlong unterminated CSI is discarded so input cannot stall")
    func overlongCSI() {
        var parser = InputParser()
        let junk = [0x1B, UInt8(ascii: "[")] + [UInt8](repeating: UInt8(ascii: "1"), count: 80)
        #expect(parser.feed(junk).isEmpty)
        #expect(!parser.hasPendingInput)
        #expect(parser.feed(Array("q".utf8)) == [.key(KeyEvent(.character("q")))])
    }

    @Test("every split point of a mixed stream yields the same events")
    func splitInvariance() {
        let stream = Array("a\u{1B}[A中\u{1B}[<0;5;6M\u{1B}[200~paste\u{1B}[201~\u{1B}[1;5Cz".utf8)
        var reference = InputParser()
        let expected = reference.feed(stream)
        #expect(expected.count == 7)
        for split in 1..<stream.count {
            var parser = InputParser()
            let events = parser.feed(Array(stream[..<split])) + parser.feed(Array(stream[split...]))
            #expect(events == expected, "split at \(split)")
        }
    }
}

@Suite("Input parser: mouse and paste")
struct InputParserMouseTests {
    func parse(_ text: String) -> [InputEvent] {
        var parser = InputParser()
        return parser.feed(Array(text.utf8))
    }

    @Test("SGR mouse presses, releases, drags and moves with zero-based coordinates")
    func sgr() {
        #expect(parse("\u{1B}[<0;10;5M") == [.mouse(MouseEvent(button: .left, action: .press, x: 9, y: 4))])
        #expect(parse("\u{1B}[<0;10;5m") == [.mouse(MouseEvent(button: .left, action: .release, x: 9, y: 4))])
        #expect(parse("\u{1B}[<2;1;1M") == [.mouse(MouseEvent(button: .right, action: .press, x: 0, y: 0))])
        #expect(parse("\u{1B}[<1;300;200M") == [.mouse(MouseEvent(button: .middle, action: .press, x: 299, y: 199))])
        #expect(parse("\u{1B}[<32;3;3M") == [.mouse(MouseEvent(button: .left, action: .drag, x: 2, y: 2))])
        #expect(parse("\u{1B}[<35;3;3M") == [.mouse(MouseEvent(button: .none, action: .move, x: 2, y: 2))])
    }

    @Test("wheel and modifier bits")
    func wheel() {
        #expect(parse("\u{1B}[<64;2;2M") == [.mouse(MouseEvent(button: .wheelUp, action: .press, x: 1, y: 1))])
        #expect(parse("\u{1B}[<65;2;2M") == [.mouse(MouseEvent(button: .wheelDown, action: .press, x: 1, y: 1))])
        #expect(parse("\u{1B}[<66;2;2M") == [.mouse(MouseEvent(button: .wheelLeft, action: .press, x: 1, y: 1))])
        #expect(parse("\u{1B}[<16;2;2M") == [.mouse(MouseEvent(button: .left, action: .press, x: 1, y: 1, modifiers: .ctrl))])
        #expect(parse("\u{1B}[<12;2;2M") == [.mouse(MouseEvent(button: .left, action: .press, x: 1, y: 1,
                                                                 modifiers: [.shift, .alt]))])
        #expect(MouseEvent(button: .wheelDown, action: .press, x: 0, y: 0).isWheel)
        #expect(!MouseEvent(button: .left, action: .press, x: 0, y: 0).isWheel)
    }

    @Test("legacy X10 mouse reports")
    func x10() {
        let press: [UInt8] = [0x1B, 0x5B, 0x4D, 32, 33 + 4, 33 + 2]
        var parser = InputParser()
        #expect(parser.feed(press) == [.mouse(MouseEvent(button: .left, action: .press, x: 4, y: 2))])
        let release: [UInt8] = [0x1B, 0x5B, 0x4D, 35, 33, 33]
        #expect(parser.feed(release) == [.mouse(MouseEvent(button: .none, action: .release, x: 0, y: 0))])
        #expect(parser.feed([0x1B, 0x5B, 0x4D, 32]).isEmpty, "waits for all three payload bytes")
    }

    @Test("malformed SGR mouse reports are dropped")
    func malformedMouse() {
        #expect(parse("\u{1B}[<0;5Mx") == [.key(KeyEvent(.character("x")))])
        #expect(parse("\u{1B}[<0;0;0M").isEmpty)
    }

    @Test("bracketed paste is delivered as one event, including escapes inside")
    func paste() {
        #expect(parse("\u{1B}[200~hello world\u{1B}[201~") == [.paste("hello world")])
        #expect(parse("\u{1B}[200~a\u{1B}[Ab\u{1B}[201~q") == [.paste("a\u{1B}[Ab"), .key(KeyEvent(.character("q")))])
        #expect(parse("\u{1B}[200~\u{1B}[201~") == [.paste("")])
    }

    @Test("paste content may arrive in many pieces, even splitting the terminator")
    func pastePieces() {
        var parser = InputParser()
        #expect(parser.feed(Array("\u{1B}[200~one ".utf8)).isEmpty)
        #expect(parser.feed(Array("two\u{1B}[2".utf8)).isEmpty)
        #expect(parser.feed(Array("01~".utf8)) == [.paste("one two")])
    }

    @Test("an unterminated paste is delivered on flush")
    func pasteFlush() {
        var parser = InputParser()
        _ = parser.feed(Array("\u{1B}[200~dangling".utf8))
        #expect(parser.hasPendingInput)
        #expect(parser.flush() == [.paste("dangling")])
    }

    @Test("modifier parameters decode from xterm values")
    func xtermModifiers() {
        #expect(KeyModifiers(xtermParameter: 1) == [])
        #expect(KeyModifiers(xtermParameter: 2) == .shift)
        #expect(KeyModifiers(xtermParameter: 7) == [.alt, .ctrl])
        #expect(KeyModifiers(xtermParameter: 0) == [])
        #expect(KeyEvent.char("x") == KeyEvent(.character("x")))
        #expect(KeyEvent.ctrl("c") == KeyEvent(.character("c"), modifiers: .ctrl))
    }
}
