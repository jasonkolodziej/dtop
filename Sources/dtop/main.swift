let profile = AppProfile.bootstrap

print("\(profile.name): \(profile.tagline)")
print("Bootstrap status: initialized")
print("Planned capabilities:")
for capability in profile.capabilities {
    print("- \(capability)")
}
