#!/bin/bash

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo -e "${GREEN}Installing web-content-service and web-content-client...${NC}"

# Check if cargo is installed
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}Error: cargo is not installed. Please install Rust and Cargo first.${NC}"
    echo "Visit https://rustup.rs/ for installation instructions."
    exit 1
fi

# Build the release version
echo "Building release version..."
cargo build --release
if [ $? -ne 0 ]; then
    echo -e "${RED}Error: Build failed${NC}"
    exit 1
fi

# Create the bin directory if it doesn't exist
mkdir -p "$HOME/.local/bin"

# Add ~/.local/bin to PATH if it's not already there
if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
    echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.bashrc"
    echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.zshrc"
    export PATH="$HOME/.local/bin:$PATH"
fi

# Install web-content-service (server)
SERVER_BIN_PATH="target/release/web-content-extract"
SERVER_INSTALL_PATH="$HOME/.local/bin/web-content-service"
echo "Installing web-content-service to $SERVER_INSTALL_PATH..."
cp "$SERVER_BIN_PATH" "$SERVER_INSTALL_PATH"
if [ $? -ne 0 ]; then
    echo -e "${RED}Error: Failed to copy server binary${NC}"
    exit 1
fi
chmod +x "$SERVER_INSTALL_PATH"
if [ $? -ne 0 ]; then
    echo -e "${RED}Error: Failed to make server binary executable${NC}"
    exit 1
fi

# Install web-content-client (client)
CLIENT_BIN_PATH="target/release/client"
CLIENT_INSTALL_PATH="$HOME/.local/bin/web-content-client"
echo "Installing web-content-client to $CLIENT_INSTALL_PATH..."
cp "$CLIENT_BIN_PATH" "$CLIENT_INSTALL_PATH"
if [ $? -ne 0 ]; then
    echo -e "${RED}Error: Failed to copy client binary${NC}"
    exit 1
fi
chmod +x "$CLIENT_INSTALL_PATH"
if [ $? -ne 0 ]; then
    echo -e "${RED}Error: Failed to make client binary executable${NC}"
    exit 1
fi

echo -e "${GREEN}Installation successful!${NC}"
echo "You can now use 'web-content-service' to run the server and 'web-content-client' to run the client from the command line."
echo "Example (server): web-content-service --port 50051"
echo "Example (client): web-content-client --url 'https://example.com' --output-md"

# Check if shell needs to be restarted
if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
    echo -e "\n${GREEN}Note:${NC} Please restart your shell or run:"
    echo "source ~/.bashrc  # if you use bash"
    echo "source ~/.zshrc   # if you use zsh"
fi 