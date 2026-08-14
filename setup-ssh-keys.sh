#!/bin/bash

# Default USB IP address for reMarkable 2
DEFAULT_IP="10.11.99.1"
REMARKABLE_IP="192.168.1.100"
KEY_PATH="$HOME/.ssh/id_ed25519"

echo "=== 1. Target IP Address ==="
# Prompt the user for the IP address
read -p "Enter reMarkable IP address (Press Enter for default USB: $DEFAULT_IP): " REMARKABLE_IP

# If the user left it blank, use the default IP
if [ -z "$REMARKABLE_IP" ]; then
    REMARKABLE_IP=$DEFAULT_IP
fi

echo "Using target IP: $REMARKABLE_IP"
echo ""

echo "=== 1. Checking for existing SSH key ==="
if [ ! -f "$KEY_PATH" ]; then
    echo "Generating new Ed25519 SSH key..."
    ssh-keygen -t ed25519 -N "" -f "$KEY_PATH"
else
    echo "Existing SSH key found at $KEY_PATH"
fi

echo "=== 2. Ensuring local SSH agent is running ==="
eval "$(ssh-agent -s)"
ssh-add --apple-use-keychain "$KEY_PATH" 2>/dev/null || ssh-add "$KEY_PATH"

echo "=== 3. Copying public key to reMarkable 2 ==="
echo "Note: Enter your reMarkable root password one last time when prompted."
ssh-copy-id -i "${KEY_PATH}.pub" root@"$REMARKABLE_IP"

echo "=== 4. Setting up Mac SSH configuration shortcut ==="
CONFIG_FILE="$HOME/.ssh/config"
touch "$CONFIG_FILE"

# Check if shortcut already exists
if ! grep -q "Host remarkable" "$CONFIG_FILE"; then
    cat >> "$CONFIG_FILE" <<EOF

Host remarkable
    HostName $REMARKABLE_IP
    User root
    IdentityFile $KEY_PATH
    AddKeysToAgent yes
    UseKeychain yes
EOF
    echo "Added 'remarkable' shortcut to $CONFIG_FILE"
else
    echo "Shortcut 'remarkable' already exists in $CONFIG_FILE"
fi

echo "=== Done! Testing connection... ==="
echo "Executing 'ssh remarkable' ..."
ssh remarkable "echo 'Success! Passwordless login works.'"
