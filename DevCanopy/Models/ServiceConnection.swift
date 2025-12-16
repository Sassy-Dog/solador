import Foundation
import SwiftData

@Model
final class ServiceConnection {
    var id: UUID = UUID()
    var service: ServiceType
    var accountIdentifier: String
    var accountName: String?
    var connectedAt: Date
    var lastValidated: Date?
    var isValid: Bool
    
    init(service: ServiceType, accountIdentifier: String) {
        self.service = service
        self.accountIdentifier = accountIdentifier
        self.connectedAt = Date()
        self.isValid = true
    }
}

enum ServiceType: String, Codable, CaseIterable {
    case github = "GitHub"
    case vercel = "Vercel"
    case neon = "Neon"
    case cloudflare = "Cloudflare"
    
    var icon: String {
        switch self {
        case .github:
            return "link.circle"
        case .vercel:
            return "triangle"
        case .neon:
            return "cylinder"
        case .cloudflare:
            return "cloud"
        }
    }
    
    var color: String {
        switch self {
        case .github:
            return "black"
        case .vercel:
            return "black"
        case .neon:
            return "green"
        case .cloudflare:
            return "orange"
        }
    }
}