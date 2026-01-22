const { spawn } = require('child_process');
const path = require('path');
const os = require('os');

function getBinaryPath() {
    const platform = os.platform();
    const arch = os.arch();

    let binaryName = 'portal-tech-cli';

    if (platform === 'darwin') {
        binaryName += '-macos';
    } else if (platform === 'linux') {
        binaryName += '-linux';
    } else if (platform === 'win32') {
        binaryName += '-win.exe';
    } else {
        throw new Error(`Unsupported platform: ${platform}`);
    }

    return path.join(__dirname, 'bin', binaryName);
}

const binaryPath = getBinaryPath();
const args = process.argv.slice(2);

const child = spawn(binaryPath, args, {
    stdio: 'inherit'
});

child.on('exit', (code) => {
    process.exit(code);
});

child.on('error', (err) => {
    console.error(`Failed to start PortalTech binary: ${err.message}`);
    process.exit(1);
});
