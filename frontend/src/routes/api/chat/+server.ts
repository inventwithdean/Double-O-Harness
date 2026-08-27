import { TrueForge } from '@truefoundry/trueforge-sdk';

export async function POST({ request }) {
    // Extract sessionId from the incoming request
    const { message, sessionId } = await request.json();

    const stream = new ReadableStream({
        async start(controller) {
            try {
                const client = new TrueForge({
                    baseUrl: 'http://localhost:8791',
                    timeoutInSeconds: 600,
                });

                let activeSessionId = sessionId;

                // Create a session if there isn't one.
                if (!activeSessionId) {
                    const { data: session } = await client.sessions.create({
                        agent: { name: 'mi6' }
                    });
                    activeSessionId = session.id;

                    // Send a custom SSE event to tell the frontend our new sessionId
                    const sessionEvent = JSON.stringify({ type: 'system.session_created', sessionId: activeSessionId });
                    controller.enqueue(new TextEncoder().encode(`data: ${sessionEvent}\n\n`));
                }

                // Start the Turn Stream using the active session
                const turnStream = await client.sessions.createTurnStream(activeSessionId, {
                    input: [{ type: 'user.message', content: message }],
                });

                // Stream events to the frontend
                for await (const { data: event } of turnStream.withMetadata()) {
                    const chunk = JSON.stringify(event);
                    controller.enqueue(new TextEncoder().encode(`data: ${chunk}\n\n`));
                }

                controller.close();
            } catch (error) {
                console.error("TrueForge SDK Error:", error);
                controller.error(error);
            }
        }
    });

    return new Response(stream, {
        headers: {
            'Content-Type': 'text/event-stream',
            'Cache-Control': 'no-cache',
            'Connection': 'keep-alive'
        }
    });
}