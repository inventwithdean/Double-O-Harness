// src/routes/api/download/+server.ts
export async function GET({ url }) {
    const sessionId = url.searchParams.get('sessionId');
    const turnId = url.searchParams.get('turnId');
    const path = url.searchParams.get('path');

    if (!sessionId || !turnId || !path) {
        return new Response('Missing required parameters', { status: 400 });
    }

    // This points to the internal backend URL
    const trueForgeUrl = `http://localhost:8791/api/v1/sessions/${sessionId}/turns/${turnId}/download-sandbox-file?path=${encodeURIComponent(path)}`;

    try {
        // Fetch the file from TrueForge
        const res = await fetch(trueForgeUrl);
        
        if (!res.ok) {
            throw new Error(`TrueForge download failed with status: ${res.status}`);
        }

        // Extract the filename from the end of the path (e.g., "random_image.png")
        const fileName = path.split('/').pop() || 'downloaded_file';

        // Proxy the stream back to the frontend, forcing it to act as a downloadable file
        return new Response(res.body, {
            headers: {
                'Content-Type': res.headers.get('Content-Type') || 'application/octet-stream',
                'Content-Disposition': `attachment; filename="${fileName}"`
            }
        });
    } catch (error) {
        console.error('Download proxy error:', error);
        return new Response('Failed to download file', { status: 500 });
    }
}