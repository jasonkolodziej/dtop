import Testing
@testable import dtop

@Test("Bootstrap profile includes docker management focus")
func bootstrapProfileCapabilities() {
    let profile = AppProfile.bootstrap

    #expect(profile.name == "dtop")
    #expect(profile.capabilities.contains("Docker resource management"))
    #expect(profile.capabilities.contains("Color-coded container and compose views"))
}
