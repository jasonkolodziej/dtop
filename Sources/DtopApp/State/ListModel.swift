import DtopCore

/// One selectable row in the current tab.
public enum ListItem: Sendable, Equatable {
    case container(Container, depth: Int, isLast: Bool)
    case composeProject(ComposeProject, collapsed: Bool)
    case image(DockerImage)
    case network(DockerNetwork)
    case volume(DockerVolume)

    public var id: String {
        switch self {
        case .container(let container, _, _): return container.id
        case .composeProject(let project, _): return "project:" + project.name
        case .image(let image): return image.id
        case .network(let network): return network.id
        case .volume(let volume): return "volume:" + volume.name
        }
    }

    public var container: Container? {
        if case .container(let container, _, _) = self { return container }
        return nil
    }

    public var composeProject: ComposeProject? {
        if case .composeProject(let project, _) = self { return project }
        return nil
    }
}

/// Builds filtered, sorted rows for a tab. Pure so it can be tested without a terminal.
public enum ListModel {
    public static func items(for tab: Tab, snapshot: DockerSnapshot, state: AppState) -> [ListItem] {
        let query = state.filterText.trimmingSpaces().lowercased()
        switch tab {
        case .containers:
            let containers = snapshot.containers
                .filter { state.statusFilter.matches($0.state) }
                .filter { query.isEmpty || matches($0, query: query) }
            return sortContainers(containers, snapshot: snapshot, key: state.sortKey, descending: state.sortDescending)
                .map { .container($0, depth: 0, isLast: false) }

        case .compose:
            let filtered = snapshot.containers.filter { state.statusFilter.matches($0.state) }
            let projects = ComposeProject.group(filtered).projects
            var items: [ListItem] = []
            for project in projects {
                let projectMatches = query.isEmpty || project.name.lowercased().contains(query)
                let members = project.containers.filter { projectMatches || matches($0, query: query) }
                guard !members.isEmpty else { continue }
                let collapsed = state.collapsedProjects.contains(project.name)
                items.append(.composeProject(project, collapsed: collapsed))
                if !collapsed {
                    let ordered = members.sorted { lhs, rhs in
                        let left = (lhs.composeService ?? "", lhs.composeContainerNumber ?? 0, lhs.name)
                        let right = (rhs.composeService ?? "", rhs.composeContainerNumber ?? 0, rhs.name)
                        return left < right
                    }
                    for (index, container) in ordered.enumerated() {
                        items.append(.container(container, depth: 1, isLast: index == ordered.count - 1))
                    }
                }
            }
            return items

        case .images:
            return snapshot.images
                .filter { image in
                    query.isEmpty || image.displayName.lowercased().contains(query)
                        || image.repoTags.contains { $0.lowercased().contains(query) }
                        || image.shortID.hasPrefix(query)
                }
                .sorted { lhs, rhs in
                    switch state.sortKey {
                    case .created: return state.sortDescending ? lhs.created > rhs.created : lhs.created < rhs.created
                    case .memory: return state.sortDescending ? lhs.size > rhs.size : lhs.size < rhs.size
                    default: return lhs.displayName == rhs.displayName ? lhs.id < rhs.id : lhs.displayName < rhs.displayName
                    }
                }
                .map(ListItem.image)

        case .networks:
            return snapshot.networks
                .filter { query.isEmpty || $0.name.lowercased().contains(query) || $0.driver.lowercased().contains(query) }
                .sorted { $0.name < $1.name }
                .map(ListItem.network)

        case .volumes:
            return snapshot.volumes
                .filter { volume in
                    query.isEmpty || volume.name.lowercased().contains(query) || volume.driver.lowercased().contains(query)
                        || (volume.composeProject?.lowercased().contains(query) ?? false)
                }
                .sorted { lhs, rhs in
                    // Named volumes first: anonymous hashes are rarely what people look for.
                    lhs.isAnonymous == rhs.isAnonymous ? lhs.name < rhs.name : !lhs.isAnonymous
                }
                .map(ListItem.volume)
        }
    }

    static func matches(_ container: Container, query: String) -> Bool {
        container.name.lowercased().contains(query)
            || container.image.lowercased().contains(query)
            || container.id.hasPrefix(query)
            || (container.composeProject?.lowercased().contains(query) ?? false)
            || (container.composeService?.lowercased().contains(query) ?? false)
            || container.state.rawValue == query
    }

    public static func sortContainers(_ containers: [Container], snapshot: DockerSnapshot, key: SortKey, descending: Bool) -> [Container] {
        let latest = snapshot.metrics.latest
        func compare(_ lhs: Container, _ rhs: Container) -> Bool? {
            switch key {
            case .name:
                return lhs.name == rhs.name ? nil : lhs.name < rhs.name
            case .cpu:
                let left = latest[lhs.id]?.cpuPercent ?? -1
                let right = latest[rhs.id]?.cpuPercent ?? -1
                return left == right ? nil : left < right
            case .memory:
                let left = latest[lhs.id]?.memoryUsage ?? 0
                let right = latest[rhs.id]?.memoryUsage ?? 0
                return left == right ? nil : left < right
            case .gpu:
                let left = latest[lhs.id]?.gpuPercent ?? -1
                let right = latest[rhs.id]?.gpuPercent ?? -1
                if left != right { return left < right }
                let leftMemory = latest[lhs.id]?.gpuMemory ?? 0
                let rightMemory = latest[rhs.id]?.gpuMemory ?? 0
                return leftMemory == rightMemory ? nil : leftMemory < rightMemory
            case .status:
                return lhs.state.sortRank == rhs.state.sortRank ? nil : lhs.state.sortRank < rhs.state.sortRank
            case .created:
                return lhs.created == rhs.created ? nil : lhs.created < rhs.created
            case .image:
                return lhs.image == rhs.image ? nil : lhs.image < rhs.image
            }
        }
        return containers.sorted { lhs, rhs in
            if let ordered = compare(lhs, rhs) { return descending ? !ordered : ordered }
            // Stable, deterministic tie-breakers: running first, then name, then ID.
            if lhs.state.sortRank != rhs.state.sortRank { return lhs.state.sortRank < rhs.state.sortRank }
            if lhs.name != rhs.name { return lhs.name < rhs.name }
            return lhs.id < rhs.id
        }
    }

    /// Resolves the selected row index from the remembered ID (or index fallback).
    public static func selectedIndex(in items: [ListItem], state: AppState, tab: Tab) -> Int? {
        guard !items.isEmpty else { return nil }
        if let id = state.selectedIDs[tab], let index = items.firstIndex(where: { $0.id == id }) {
            return index
        }
        let fallback = state.selectedIndexes[tab] ?? 0
        return min(max(0, fallback), items.count - 1)
    }

    public static func selectedItem(snapshot: DockerSnapshot, state: AppState) -> ListItem? {
        let rows = items(for: state.tab, snapshot: snapshot, state: state)
        return selectedIndex(in: rows, state: state, tab: state.tab).map { rows[$0] }
    }
}
