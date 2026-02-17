declare global {
    interface Window {
        SHARETTY_CONFIG?: {
            upload: boolean;
            download: boolean;
        }
    }
}

export class Overlay {
    private container: HTMLElement;
    private panel: HTMLElement;
    private sessionInfo: HTMLElement;
    private isVisible: boolean = false;
    private menuBtn: HTMLElement;

    constructor() {
        this.container = document.createElement('div');
        this.container.id = 'ui-layer';
        document.body.appendChild(this.container);

        // Create Menu Button
        this.menuBtn = document.createElement('button');
        this.menuBtn.className = 'menu-btn';
        this.menuBtn.textContent = '☰';
        this.menuBtn.onclick = () => this.toggle();
        this.container.appendChild(this.menuBtn);

        // Create Panel
        this.panel = document.createElement('div');
        this.panel.className = 'overlay-panel';
        this.container.appendChild(this.panel);

        // Panel Content
        const title = document.createElement('h2');
        title.textContent = 'Menu';
        this.panel.appendChild(title);

        const closeBtn = document.createElement('button');
        closeBtn.className = 'close-btn';
        closeBtn.textContent = '×';
        closeBtn.onclick = () => this.hide();
        this.panel.appendChild(closeBtn);

        // Session Info Section
        const infoSection = document.createElement('div');
        infoSection.className = 'info-section';
        infoSection.innerHTML = '<h3>Session</h3>';
        this.sessionInfo = document.createElement('div');
        this.sessionInfo.className = 'session-info';
        infoSection.appendChild(this.sessionInfo);
        this.panel.appendChild(infoSection);

        // File Upload Section
        if (window.SHARETTY_CONFIG?.upload) {
            const uploadSection = document.createElement('div');
            uploadSection.className = 'upload-section';
            uploadSection.innerHTML = '<h3>Upload</h3>';

            const dropZone = document.createElement('div');
            dropZone.className = 'drop-zone';
            dropZone.textContent = 'Drag files here or click to upload';
            dropZone.onclick = () => fileInput.click();

            const fileInput = document.createElement('input');
            fileInput.type = 'file';
            fileInput.multiple = true;
            fileInput.style.display = 'none';
            fileInput.onchange = (e) => this.handleUpload((e.target as HTMLInputElement).files);

            // Drag and Drop events
            dropZone.addEventListener('dragover', (e) => {
                e.preventDefault();
                dropZone.classList.add('drag-over');
            });
            dropZone.addEventListener('dragleave', () => dropZone.classList.remove('drag-over'));
            dropZone.addEventListener('drop', (e) => {
                e.preventDefault();
                dropZone.classList.remove('drag-over');
                if (e.dataTransfer && e.dataTransfer.files.length > 0) {
                    this.handleUpload(e.dataTransfer.files);
                }
            });

            uploadSection.appendChild(dropZone);
            uploadSection.appendChild(fileInput);
            this.panel.appendChild(uploadSection);
        }

        // File Download Section
        if (window.SHARETTY_CONFIG?.download) {
            const downloadSection = document.createElement('div');
            downloadSection.className = 'download-section';
            downloadSection.innerHTML = '<h3>Files</h3>';

            const downloadLink = document.createElement('a');
            downloadLink.href = 'download/';
            downloadLink.target = '_blank';
            downloadLink.className = 'download-link';
            downloadLink.textContent = 'Browse Files';

            downloadSection.appendChild(downloadLink);
            this.panel.appendChild(downloadSection);
        }

        // Close on click outside
        document.addEventListener('click', (e) => {
            if (this.isVisible && !this.panel.contains(e.target as Node) && e.target !== this.menuBtn) {
                this.hide();
            }
        });
    }

    toggle() {
        if (this.isVisible) this.hide();
        else this.show();
    }

    show() {
        this.panel.classList.add('visible');
        this.isVisible = true;
    }

    hide() {
        this.panel.classList.remove('visible');
        this.isVisible = false;
    }

    async handleUpload(files: FileList | null) {
        if (!files || files.length === 0) return;

        const formData = new FormData();
        for (let i = 0; i < files.length; i++) {
            formData.append('file', files[i]);
        }

        // Try relative 'upload' first
        try {
            const res = await fetch('upload', {
                method: 'POST',
                body: formData
            });
            if (res.ok) {
                alert('Upload successful!');
            } else {
                console.error('Upload failed', res);
                alert('Upload failed');
            }
        } catch (e) {
            console.error(e);
            alert('Upload error');
        }
    }

    updateStatus(connected: boolean) {
        const ssid = window.location.pathname.split('/s/')[1]?.replace('/', '') || 'Direct';
        const color = connected ? '#4caf50' : '#f44336';
        const statusText = connected ? 'Connected' : 'Disconnected';

        this.sessionInfo.innerHTML = `
            <div style="display: flex; justify-content: space-between; margin-bottom: 5px;">
                <span>Status:</span>
                <span style="color: ${color}; font-weight: bold;">${statusText}</span>
            </div>
            <div style="display: flex; justify-content: space-between;">
                <span>Session ID:</span>
                <span style="color: #ccc;">${ssid}</span>
            </div>
        `;
    }
}
