import Foundation
import Security

enum KeychainError: LocalizedError, Equatable {
    case duplicateItem
    case itemNotFound
    case unexpectedStatus(OSStatus)
    case invalidData
    
    var errorDescription: String? {
        switch self {
        case .duplicateItem:
            return "Item already exists in keychain"
        case .itemNotFound:
            return "Item not found in keychain"
        case .unexpectedStatus(let status):
            return "Keychain error: \(status)"
        case .invalidData:
            return "Invalid data format"
        }
    }
}

final class KeychainHelper {
    static let shared = KeychainHelper()
    
    private let serviceName = "com.sassydog.devcanopy"
    
    private init() {}
    
    // MARK: - Generic Methods
    
    func save(_ data: Data, for key: String) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: key,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        ]
        
        // First try to delete any existing item
        SecItemDelete(query as CFDictionary)
        
        // Add the new item
        let status = SecItemAdd(query as CFDictionary, nil)
        
        guard status == errSecSuccess else {
            if status == errSecDuplicateItem {
                throw KeychainError.duplicateItem
            }
            throw KeychainError.unexpectedStatus(status)
        }
    }
    
    func load(for key: String) throws -> Data {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: key,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        
        guard status == errSecSuccess else {
            if status == errSecItemNotFound {
                throw KeychainError.itemNotFound
            }
            throw KeychainError.unexpectedStatus(status)
        }
        
        guard let data = result as? Data else {
            throw KeychainError.invalidData
        }
        
        return data
    }
    
    func delete(for key: String) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceName,
            kSecAttrAccount as String: key
        ]
        
        let status = SecItemDelete(query as CFDictionary)
        
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainError.unexpectedStatus(status)
        }
    }
    
    // MARK: - Convenience Methods
    
    func saveString(_ string: String, for key: String) throws {
        guard let data = string.data(using: .utf8) else {
            throw KeychainError.invalidData
        }
        try save(data, for: key)
    }
    
    func loadString(for key: String) throws -> String {
        let data = try load(for: key)
        guard let string = String(data: data, encoding: .utf8) else {
            throw KeychainError.invalidData
        }
        return string
    }
    
    // MARK: - Service-Specific Methods
    
    func saveGitHubToken(_ token: String) throws {
        try saveString(token, for: "github_access_token")
    }
    
    func loadGitHubToken() -> String? {
        try? loadString(for: "github_access_token")
    }
    
    func deleteGitHubToken() {
        try? delete(for: "github_access_token")
    }
    
    // Per-host agent bearer tokens (keyed by MonitoredHost id).
    func saveHostToken(_ token: String, hostID: UUID) throws {
        try saveString(token, for: "host_token_\(hostID.uuidString)")
    }

    func loadHostToken(hostID: UUID) -> String? {
        try? loadString(for: "host_token_\(hostID.uuidString)")
    }

    func deleteHostToken(hostID: UUID) {
        try? delete(for: "host_token_\(hostID.uuidString)")
    }

    func saveVercelToken(_ token: String) throws {
        try saveString(token, for: "vercel_access_token")
    }
    
    func loadVercelToken() -> String? {
        try? loadString(for: "vercel_access_token")
    }
    
    func deleteVercelToken() {
        try? delete(for: "vercel_access_token")
    }
}