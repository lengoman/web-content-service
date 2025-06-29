#!/bin/bash

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m' # No Color

# Check for Node.js and npx
if ! command -v node &> /dev/null; then
    echo -e "${RED}Error: Node.js is not installed. Please install Node.js first.${NC}"
    exit 1
fi
if ! command -v npx &> /dev/null; then
    echo -e "${RED}Error: npx is not installed. Please install Node.js (which includes npx) first.${NC}"
    exit 1
fi

echo -e "${GREEN}Installing ms-playwright Chromium browser...${NC}"
npx playwright install chromium
if [ $? -ne 0 ]; then
    echo -e "${RED}Error: Failed to install Chromium with Playwright.${NC}"
    exit 1
fi

echo -e "${GREEN}Chromium installation complete!${NC}"
echo "You can now run the web-content-service." 