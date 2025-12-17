import SwiftUI
import SwiftData

struct GitHubAuthenticationView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.modelContext) private var modelContext
    @EnvironmentObject private var githubService: GitHubService
    
    @State private var authMethod: AuthMethod = .personalAccessToken
    @State private var personalAccessToken = ""
    @State private var isAuthenticating = false
    @State private var errorMessage: String?
    
    enum AuthMethod {
        case oauth
        case personalAccessToken
    }
    
    var body: some View {
        VStack(spacing: 0) {
            // Header
            VStack(spacing: 16) {
                Image(systemName: "logo.github")
                    .font(.system(size: 60))
                    .foregroundColor(.primary)
                
                Text("Connect to GitHub")
                    .font(.title)
                    .bold()
                
                Text("Choose how you'd like to authenticate")
                    .foregroundColor(.secondary)
            }
            .padding(.vertical, 30)
            
            Divider()
            
            // Auth Method Selection
            VStack(spacing: 20) {
                Picker("Authentication Method", selection: $authMethod) {
                    Text("OAuth (Recommended)").tag(AuthMethod.oauth)
                    Text("Personal Access Token").tag(AuthMethod.personalAccessToken)
                }
                .pickerStyle(.segmented)
                .padding(.horizontal)
                
                // Content based on selected method
                if authMethod == .oauth {
                    OAuthAuthenticationView(
                        isAuthenticating: $isAuthenticating,
                        errorMessage: $errorMessage
                    )
                } else {
                    PersonalAccessTokenView(
                        token: $personalAccessToken,
                        isAuthenticating: $isAuthenticating,
                        errorMessage: $errorMessage
                    )
                }
                
                // Error display
                if let error = errorMessage {
                    HStack {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundColor(.red)
                        Text(error)
                            .foregroundColor(.red)
                            .font(.caption)
                        Spacer()
                    }
                    .padding(.horizontal)
                    .padding(.vertical, 8)
                    .background(Color.red.opacity(0.1))
                    .cornerRadius(8)
                    .padding(.horizontal)
                }
            }
            .padding(.vertical, 20)
            
            Spacer()
            
            Divider()
            
            // Footer buttons
            HStack {
                Button("Cancel") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                
                Spacer()
                
                if authMethod == .oauth {
                    Button("Authenticate with GitHub") {
                        authenticateWithOAuth()
                    }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(isAuthenticating)
                } else {
                    Button("Connect") {
                        authenticateWithPAT()
                    }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(personalAccessToken.isEmpty || isAuthenticating)
                }
            }
            .padding()
        }
        .frame(width: 600, height: 550)
    }
    
    private func authenticateWithOAuth() {
        isAuthenticating = true
        errorMessage = nil
        
        Task {
            do {
                try await githubService.authenticate()
            } catch {
                await MainActor.run {
                    errorMessage = error.localizedDescription
                    isAuthenticating = false
                }
            }
        }
    }
    
    private func authenticateWithPAT() {
        isAuthenticating = true
        errorMessage = nil
        
        Task {
            do {
                // Set the token in the service
                await MainActor.run {
                    githubService.setAccessToken(personalAccessToken)
                }
                
                // Test the token by fetching user info
                try await githubService.fetchCurrentUser()
                
                // If successful, save the connection
                await MainActor.run {
                    if let user = githubService.currentUser {
                        let connection = ServiceConnection(
                            service: .github,
                            accountIdentifier: user.login
                        )
                        connection.accountName = user.name ?? user.login
                        modelContext.insert(connection)
                        try? modelContext.save()
                        
                        dismiss()
                    }
                }
            } catch {
                await MainActor.run {
                    errorMessage = "Invalid token: \(error.localizedDescription)"
                    isAuthenticating = false
                }
            }
        }
    }
}

struct OAuthAuthenticationView: View {
    @Binding var isAuthenticating: Bool
    @Binding var errorMessage: String?
    
    var body: some View {
        VStack(spacing: 20) {
            VStack(alignment: .leading, spacing: 12) {
                Label("Secure OAuth 2.0 Authentication", systemImage: "lock.shield.fill")
                    .font(.headline)
                
                Text("You'll be redirected to GitHub to authorize DevCanopy. This is the most secure method and doesn't require sharing your credentials.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                
                VStack(alignment: .leading, spacing: 8) {
                    Text("DevCanopy will request access to:")
                        .font(.caption)
                        .fontWeight(.medium)
                    
                    Label("Read repository information", systemImage: "book.closed")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    Label("Read workflow and action status", systemImage: "arrow.triangle.2.circlepath")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                .padding()
                .background(Color(NSColor.controlBackgroundColor))
                .cornerRadius(8)
            }
            .padding(.horizontal)
            
            if isAuthenticating {
                HStack {
                    ProgressView()
                        .scaleEffect(0.8)
                    Text("Waiting for authentication...")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
        }
    }
}

struct PersonalAccessTokenView: View {
    @Binding var token: String
    @Binding var isAuthenticating: Bool
    @Binding var errorMessage: String?
    @State private var showTokenHelp = false
    
    var body: some View {
        VStack(spacing: 20) {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Label("Personal Access Token", systemImage: "key.fill")
                        .font(.headline)
                    
                    Spacer()
                    
                    Button(action: { showTokenHelp = true }) {
                        Image(systemName: "questionmark.circle")
                            .foregroundColor(.secondary)
                    }
                    .buttonStyle(.plain)
                    .popover(isPresented: $showTokenHelp) {
                        TokenHelpView()
                    }
                }
                
                Text("Use a GitHub Personal Access Token for authentication. This method is good for testing but OAuth is recommended for production use.")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                
                SecureField("ghp_xxxxxxxxxxxxxxxxxxxx", text: $token)
                    .textFieldStyle(.roundedBorder)
                
                VStack(alignment: .leading, spacing: 8) {
                    Text("Required scopes:")
                        .font(.caption)
                        .fontWeight(.medium)
                    
                    Label("repo (Full control of private repositories)", systemImage: "checkmark.circle.fill")
                        .font(.caption)
                        .foregroundColor(.secondary)
                    
                    Label("workflow (Update GitHub Action workflows)", systemImage: "checkmark.circle.fill")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
                .padding()
                .background(Color(NSColor.controlBackgroundColor))
                .cornerRadius(8)
                
                Button("Create a Personal Access Token on GitHub") {
                    if let url = URL(string: "https://github.com/settings/tokens/new?scopes=repo,workflow&description=DevCanopy") {
                        NSWorkspace.shared.open(url)
                    }
                }
                .buttonStyle(.link)
                .font(.caption)
            }
            .padding(.horizontal)
            
            if isAuthenticating {
                HStack {
                    ProgressView()
                        .scaleEffect(0.8)
                    Text("Verifying token...")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
        }
    }
}

struct TokenHelpView: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("How to create a Personal Access Token")
                .font(.headline)
            
            VStack(alignment: .leading, spacing: 8) {
                Label("Go to GitHub Settings > Developer settings", systemImage: "1.circle.fill")
                Label("Click 'Personal access tokens' > 'Tokens (classic)'", systemImage: "2.circle.fill")
                Label("Click 'Generate new token'", systemImage: "3.circle.fill")
                Label("Name it 'DevCanopy' and select scopes: repo, workflow", systemImage: "4.circle.fill")
                Label("Copy the token and paste it here", systemImage: "5.circle.fill")
            }
            .font(.caption)
            
            Text("Note: The token will only be shown once. Store it securely.")
                .font(.caption2)
                .foregroundColor(.orange)
        }
        .padding()
        .frame(width: 350)
    }
}

#Preview {
    GitHubAuthenticationView()
        .environmentObject(GitHubService.shared)
}