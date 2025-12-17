import Foundation
import SwiftData
import Combine

@MainActor
final class GitMonitorService: ObservableObject {
    
    private var modelContext: ModelContext
    private var repositoryMonitors: [UUID: RepositoryMonitor] = [:]
    private var updateTimer: Timer?
    private var workflowUpdateTimer: Timer?
    
    // Update intervals (in seconds)
    private let quickUpdateInterval: TimeInterval = 30
    private let normalUpdateInterval: TimeInterval = 60
    private let slowUpdateInterval: TimeInterval = 300 // 5 minutes
    private let workflowUpdateInterval: TimeInterval = 180 // 3 minutes for GitHub Actions
    
    init(modelContext: ModelContext) {
        self.modelContext = modelContext
        startUpdateTimer()
        startWorkflowUpdateTimer()
        
        appLogger.info("GitMonitorService initialized")
    }
    
    deinit {
        Task { @MainActor in
            stopAllMonitors()
            updateTimer?.invalidate()
            workflowUpdateTimer?.invalidate()
        }
    }
    
    // MARK: - Public Methods
    
    func startMonitoring(_ repository: TrackedRepository) {
        guard repositoryMonitors[repository.id] == nil else {
            appLogger.debug("Already monitoring repository: \(repository.name)")
            return
        }
        
        let monitor = RepositoryMonitor(
            repositoryId: repository.id,
            path: repository.path
        ) { [weak self] in
            Task { @MainActor in
                await self?.updateRepositoryStatus(repository)
            }
        }
        
        repositoryMonitors[repository.id] = monitor
        monitor.start()
        
        // Perform initial status check
        Task {
            await updateRepositoryStatus(repository)
        }
        
        appLogger.info("Started monitoring repository: \(repository.name)")
    }
    
    func stopMonitoring(_ repository: TrackedRepository) {
        if let monitor = repositoryMonitors.removeValue(forKey: repository.id) {
            monitor.stop()
            appLogger.info("Stopped monitoring repository: \(repository.name)")
        }
    }
    
    func refreshRepository(_ repository: TrackedRepository) async {
        await updateRepositoryStatus(repository)
    }
    
    func refreshAllRepositories() async {
        let descriptor = FetchDescriptor<TrackedRepository>()
        do {
            let repositories = try modelContext.fetch(descriptor)
            await withTaskGroup(of: Void.self) { group in
                for repository in repositories {
                    group.addTask { [weak self] in
                        await self?.updateRepositoryStatus(repository)
                    }
                }
            }
        } catch {
            appLogger.error("Failed to fetch repositories: \(error)")
        }
    }
    
    // MARK: - Private Methods
    
    private func startUpdateTimer() {
        // Use normal interval by default, can be made configurable
        updateTimer = Timer.scheduledTimer(withTimeInterval: normalUpdateInterval, repeats: true) { [weak self] _ in
            Task { @MainActor in
                await self?.refreshAllRepositories()
            }
        }
    }
    
    private func startWorkflowUpdateTimer() {
        workflowUpdateTimer = Timer.scheduledTimer(withTimeInterval: workflowUpdateInterval, repeats: true) { [weak self] _ in
            Task { @MainActor in
                await self?.refreshAllWorkflows()
            }
        }
    }
    
    private func stopAllMonitors() {
        for monitor in repositoryMonitors.values {
            monitor.stop()
        }
        repositoryMonitors.removeAll()
    }
    
    private func updateRepositoryStatus(_ repository: TrackedRepository) async {
        let gitCommand = GitCommand(workingDirectory: repository.path)
        
        // Get current status
        let status = await gitCommand.getStatus()
        
        // Update repository properties
        repository.currentBranch = status.branch
        repository.trackingBranch = status.trackingBranch
        repository.hasUncommittedChanges = status.hasUncommittedChanges
        repository.aheadCount = status.aheadCount
        repository.behindCount = status.behindCount
        repository.lastChecked = Date()
        
        // Auto-detect GitHub repository if not set
        if repository.githubRepoIdentifier == nil {
            if let detectedGitHubRepo = await gitCommand.getGitHubRepoIdentifier() {
                repository.githubRepoIdentifier = detectedGitHubRepo
                appLogger.info("Auto-detected GitHub repository: \(detectedGitHubRepo) for \(repository.name)")
                
                // Trigger workflow fetch for newly detected GitHub repo
                Task {
                    await updateWorkflowStatus(for: repository, using: GitHubService(modelContext: modelContext))
                }
            }
        }
        
        // Save changes
        do {
            try modelContext.save()
            appLogger.debug("Updated status for repository: \(repository.name)")
        } catch {
            appLogger.error("Failed to save repository status: \(error)")
        }
    }
    
    // MARK: - GitHub Workflow Methods
    
    func refreshAllWorkflows() async {
        let descriptor = FetchDescriptor<TrackedRepository>(
            predicate: #Predicate<TrackedRepository> { repo in
                repo.githubRepoIdentifier != nil
            }
        )
        
        do {
            let repositories = try modelContext.fetch(descriptor)
            let githubService = GitHubService(modelContext: modelContext)
            
            await withTaskGroup(of: Void.self) { group in
                for repository in repositories {
                    group.addTask { [weak self] in
                        await self?.updateWorkflowStatus(for: repository, using: githubService)
                    }
                }
            }
        } catch {
            appLogger.error("Failed to fetch repositories for workflow update: \(error)")
        }
    }
    
    func refreshWorkflows(for repository: TrackedRepository) async {
        let githubService = GitHubService(modelContext: modelContext)
        await updateWorkflowStatus(for: repository, using: githubService)
    }
    
    private func updateWorkflowStatus(for repository: TrackedRepository, using githubService: GitHubService) async {
        guard repository.githubRepoIdentifier != nil else { return }
        
        do {
            // If no workflows are configured, fetch them
            if repository.githubWorkflows?.isEmpty ?? true {
                let workflows = try await githubService.fetchWorkflows(for: repository)
                
                // Associate workflows with repository
                for workflow in workflows {
                    workflow.repository = repository
                    modelContext.insert(workflow)
                }
                
                try modelContext.save()
                appLogger.info("Fetched \(workflows.count) workflows for \(repository.name)")
            }
            
            // Fetch latest runs for active workflows
            let workflows = repository.activeWorkflows
            if !workflows.isEmpty {
                for workflow in workflows {
                    let runs = try await githubService.fetchWorkflowRuns(
                        for: workflow,
                        repository: repository,
                        limit: 5
                    )
                    
                    // Update workflow with latest runs
                    workflow.runs = runs
                    workflow.lastFetchedAt = Date()
                    
                    // Associate runs with workflow
                    for run in runs {
                        run.workflow = workflow
                    }
                }
                
                try modelContext.save()
                appLogger.debug("Updated workflow runs for \(repository.name)")
            }
        } catch {
            appLogger.error("Failed to update workflows for \(repository.name): \(error)")
        }
    }
}

// MARK: - Repository Monitor

private final class RepositoryMonitor {
    private let repositoryId: UUID
    private let path: String
    private var stream: FSEventStreamRef?
    private let callback: () -> Void
    private var lastEventTime = Date()
    private let debounceInterval: TimeInterval = 2.0 // Debounce file system events
    
    init(repositoryId: UUID, path: String, callback: @escaping () -> Void) {
        self.repositoryId = repositoryId
        self.path = path
        self.callback = callback
    }
    
    func start() {
        let gitPath = URL(fileURLWithPath: path).appendingPathComponent(".git").path
        let pathsToWatch = [path as CFString, gitPath as CFString] as CFArray
        
        var context = FSEventStreamContext()
        context.info = Unmanaged.passUnretained(self).toOpaque()
        
        let flags = UInt32(kFSEventStreamCreateFlagUseCFTypes |
                          kFSEventStreamCreateFlagFileEvents |
                          kFSEventStreamCreateFlagNoDefer)
        
        stream = FSEventStreamCreate(
            kCFAllocatorDefault,
            { (stream, clientCallbackInfo, numEvents, eventPaths, eventFlags, eventIds) in
                let monitor = Unmanaged<RepositoryMonitor>.fromOpaque(clientCallbackInfo!).takeUnretainedValue()
                monitor.handleEvents()
            },
            &context,
            pathsToWatch,
            FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
            0.5, // latency in seconds
            FSEventStreamCreateFlags(flags)
        )
        
        if let stream = stream {
            let queue = DispatchQueue(label: "com.sassydog.devcanopy.fsevents.\(repositoryId)")
            FSEventStreamSetDispatchQueue(stream, queue)
            FSEventStreamStart(stream)
        }
    }
    
    func stop() {
        if let stream = stream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
            self.stream = nil
        }
    }
    
    private func handleEvents() {
        let now = Date()
        if now.timeIntervalSince(lastEventTime) > debounceInterval {
            lastEventTime = now
            callback()
        }
    }
    
    deinit {
        stop()
    }
}

// MARK: - Git Command Execution

private struct GitCommand {
    let workingDirectory: String
    
    func getStatus() async -> GitStatusParser.GitStatus {
        // Get full status for better parsing
        let statusOutput = await executeGit(["status", "--porcelain=v2", "--branch"])
        
        // Get ahead/behind counts
        var revListOutput: String?
        if let branch = parseCurrentBranch(from: statusOutput),
           let upstream = await getUpstreamBranch(for: branch) {
            revListOutput = await executeGit(["rev-list", "--left-right", "--count", "\(branch)...\(upstream)"])
        }
        
        return GitStatusParser.parseStatus(from: statusOutput, revListOutput: revListOutput)
    }
    
    func getGitHubRepoIdentifier() async -> String? {
        // Get all remotes
        let remotesOutput = await executeGit(["remote", "-v"])
        let lines = remotesOutput.components(separatedBy: .newlines)
        
        // Look for origin first, then any GitHub remote
        var githubUrls: [(name: String, url: String)] = []
        
        for line in lines {
            let components = line.split(separator: "\t", maxSplits: 1).map(String.init)
            guard components.count >= 2 else { continue }
            
            let remoteName = components[0]
            let urlPart = components[1].split(separator: " ").first.map(String.init) ?? ""
            
            if urlPart.contains("github.com") {
                githubUrls.append((name: remoteName, url: urlPart))
            }
        }
        
        // Prefer origin
        let targetUrl = githubUrls.first(where: { $0.name == "origin" })?.url ?? githubUrls.first?.url
        
        guard let url = targetUrl else { return nil }
        
        // Parse GitHub URL formats
        // SSH: git@github.com:owner/repo.git
        // HTTPS: https://github.com/owner/repo.git or https://github.com/owner/repo
        
        if url.contains("git@github.com:") {
            // SSH format
            let repoPath = url
                .replacingOccurrences(of: "git@github.com:", with: "")
                .replacingOccurrences(of: ".git", with: "")
            return repoPath.isEmpty ? nil : repoPath
        } else if url.contains("github.com/") {
            // HTTPS format
            let pattern = #"github\.com/([^/]+/[^/\.]+)"#
            if let regex = try? NSRegularExpression(pattern: pattern, options: []),
               let match = regex.firstMatch(in: url, options: [], range: NSRange(url.startIndex..., in: url)) {
                if let range = Range(match.range(at: 1), in: url) {
                    return String(url[range])
                }
            }
        }
        
        return nil
    }
    
    private func parseCurrentBranch(from statusOutput: String) -> String? {
        let lines = statusOutput.components(separatedBy: .newlines)
        for line in lines {
            // Parse from porcelain v2 format
            if line.hasPrefix("# branch.head ") {
                let head = line.dropFirst("# branch.head ".count).trimmingCharacters(in: .whitespaces)
                return head == "(detached)" ? nil : head
            }
        }
        return nil
    }
    
    private func getUpstreamBranch(for branch: String) async -> String? {
        let output = await executeGit(["rev-parse", "--abbrev-ref", "\(branch)@{upstream}"])
        let trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
    
    private func executeGit(_ arguments: [String]) async -> String {
        await withCheckedContinuation { continuation in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
            process.currentDirectoryURL = URL(fileURLWithPath: workingDirectory)
            process.arguments = arguments
            
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            
            do {
                try process.run()
                process.waitUntilExit()
                
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                let output = String(data: data, encoding: .utf8) ?? ""
                continuation.resume(returning: output)
            } catch {
                appLogger.error("Git command failed: \(error)")
                continuation.resume(returning: "")
            }
        }
    }
}