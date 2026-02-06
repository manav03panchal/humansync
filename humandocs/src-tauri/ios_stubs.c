// Stub symbols for SystemConfiguration APIs that are deprecated/missing on iOS.
// The system-configuration crate v0.6 references these unconditionally, but
// SCNetworkConfiguration APIs don't exist in the iOS SDK. These stubs prevent
// linker errors. They are never actually called at runtime on iOS.

#include <CoreFoundation/CoreFoundation.h>
#include <stdint.h>
#include <stdbool.h>

// --- SCNetworkInterface type constants (deprecated on iOS) ---

const CFStringRef kSCNetworkInterfaceType6to4 = CFSTR("6to4");
const CFStringRef kSCNetworkInterfaceTypeBluetooth = CFSTR("Bluetooth");
const CFStringRef kSCNetworkInterfaceTypeBond = CFSTR("Bond");
const CFStringRef kSCNetworkInterfaceTypeEthernet = CFSTR("Ethernet");
const CFStringRef kSCNetworkInterfaceTypeFireWire = CFSTR("FireWire");
const CFStringRef kSCNetworkInterfaceTypeIEEE80211 = CFSTR("IEEE80211");
const CFStringRef kSCNetworkInterfaceTypeIPSec = CFSTR("IPSec");
const CFStringRef kSCNetworkInterfaceTypeIPv4 = CFSTR("IPv4");
const CFStringRef kSCNetworkInterfaceTypeL2TP = CFSTR("L2TP");
const CFStringRef kSCNetworkInterfaceTypeModem = CFSTR("Modem");
const CFStringRef kSCNetworkInterfaceTypePPP = CFSTR("PPP");
const CFStringRef kSCNetworkInterfaceTypePPTP = CFSTR("PPTP");
const CFStringRef kSCNetworkInterfaceTypeSerial = CFSTR("Serial");
const CFStringRef kSCNetworkInterfaceTypeVLAN = CFSTR("VLAN");
const CFStringRef kSCNetworkInterfaceTypeWWAN = CFSTR("WWAN");

// --- SCNetworkInterface functions (macOS-only) ---

typedef const void *SCNetworkInterfaceRef;

CFArrayRef SCNetworkInterfaceCopyAll(void) { return CFArrayCreate(NULL, NULL, 0, NULL); }
CFStringRef SCNetworkInterfaceGetBSDName(SCNetworkInterfaceRef iface) { (void)iface; return NULL; }
CFStringRef SCNetworkInterfaceGetInterfaceType(SCNetworkInterfaceRef iface) { (void)iface; return NULL; }
CFStringRef SCNetworkInterfaceGetLocalizedDisplayName(SCNetworkInterfaceRef iface) { (void)iface; return NULL; }

// --- SCNetworkService functions (macOS-only) ---

typedef const void *SCNetworkServiceRef;

CFArrayRef SCNetworkServiceCopyAll(const void *prefs) { (void)prefs; return CFArrayCreate(NULL, NULL, 0, NULL); }
bool SCNetworkServiceGetEnabled(SCNetworkServiceRef service) { (void)service; return false; }
SCNetworkInterfaceRef SCNetworkServiceGetInterface(SCNetworkServiceRef service) { (void)service; return NULL; }
CFStringRef SCNetworkServiceGetServiceID(SCNetworkServiceRef service) { (void)service; return NULL; }
// Alias used by the crate
CFArrayRef SCNetworkServiceCopyInterface(SCNetworkServiceRef service) { (void)service; return NULL; }

// --- SCNetworkSet functions (macOS-only) ---

typedef const void *SCNetworkSetRef;

SCNetworkSetRef SCNetworkSetCopyCurrent(const void *prefs) { (void)prefs; return NULL; }
CFArrayRef SCNetworkSetGetServiceOrder(SCNetworkSetRef set) { (void)set; return NULL; }

// --- SCPreferences (macOS-only) ---

typedef const void *SCPreferencesRef;

SCPreferencesRef SCPreferencesCreate(CFAllocatorRef alloc, CFStringRef name, CFStringRef prefsID) {
    (void)alloc; (void)name; (void)prefsID;
    return NULL;
}
