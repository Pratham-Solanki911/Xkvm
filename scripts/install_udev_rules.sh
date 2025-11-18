#!/bin/bash

# Installation script for udev rules on Linux
# This script allows non-root users to access /dev/uinput for input injection

set -e

echo "KVM-RS: Installing udev rules for input device access"

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo "This script must be run as root (use sudo)"
    exit 1
fi

# Create udev rule
cat > /etc/udev/rules.d/99-kvm-rs.rules << 'EOF'
# Allow members of the input group to access uinput
KERNEL=="uinput", MODE="0660", GROUP="input", OPTIONS+="static_node=uinput"

# Alternative: create uinput device if it doesn't exist
KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"
EOF

echo "Udev rule created at /etc/udev/rules.d/99-kvm-rs.rules"

# Reload udev rules
echo "Reloading udev rules..."
udevadm control --reload-rules
udevadm trigger

# Load uinput module
echo "Loading uinput kernel module..."
modprobe uinput || echo "Warning: Could not load uinput module (may already be loaded)"

# Add uinput to modules to load on boot
if [ ! -f /etc/modules-load.d/uinput.conf ]; then
    echo "uinput" > /etc/modules-load.d/uinput.conf
    echo "Added uinput to modules-load.d for automatic loading on boot"
fi

echo ""
echo "Installation complete!"
echo ""
echo "To allow your user to access input devices, add them to the 'input' group:"
echo "  sudo usermod -a -G input \$USER"
echo ""
echo "Then log out and log back in for changes to take effect."
echo ""
