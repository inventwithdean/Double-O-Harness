<script lang="ts">
	type ToolCall = {
		index: number;
		id: string;
		name: string;
		args: string;
		result: string;
		status: 'streaming' | 'done';
	};

	type SubMessage = {
		id: string;
		content: string;
		tools: ToolCall[];
	};

	type TimelineBlock = {
		id: string;
		threadId: string;
		title: string;
		isMain: boolean;
		status: 'running' | 'done' | 'error';
		subMessages: SubMessage[];
	};

	type Message = {
		role: 'user' | 'agent';
		content: string;
		turnId?: string;
		blocks: TimelineBlock[];
	};

	let sessionId = $state('');
	let messages = $state<Message[]>([]);
	let currentInput = $state('');
	let isThinking = $state(false);

	// Tracks active threads so we know their titles when they resume
	let threadMeta = $state<Record<string, { title: string; status: 'running' | 'done' }>>({});

	// Helper to safely format JSON while it might still be streaming
	function formatJson(raw: string) {
		if (!raw) return '{}';
		try {
			return JSON.stringify(JSON.parse(raw), null, 2);
		} catch (e) {
			return raw;
		}
	}

	// Helper to extract file paths from ```sandbox_artifacts markdown
	function parseContentBlocks(text: string) {
		if (!text) return [];

		const regex = /```sandbox_artifacts\n([\s\S]*?)```/g;
		const parts = [];
		let lastIndex = 0;
		let match;

		while ((match = regex.exec(text)) !== null) {
			if (match.index > lastIndex) {
				parts.push({ type: 'text', content: text.slice(lastIndex, match.index) });
			}

			const linksText = match[1];
			const linkRegex = /\[([^\]]+)\]\(([^)]+)\)/g;
			const artifacts = [];
			let linkMatch;

			while ((linkMatch = linkRegex.exec(linksText)) !== null) {
				artifacts.push({ name: linkMatch[1], path: linkMatch[2] });
			}

			parts.push({ type: 'artifacts', items: artifacts });
			lastIndex = regex.lastIndex;
		}

		if (lastIndex < text.length) {
			parts.push({ type: 'text', content: text.slice(lastIndex) });
		}

		return parts;
	}

	async function sendMessage() {
		if (!currentInput.trim()) return;

		const payload = currentInput;
		// User message uses standard content
		messages = [...messages, { role: 'user', content: payload, blocks: [] }];
		currentInput = '';
		isThinking = true;

		// Reset thread metadata for the new turn
		threadMeta = { main: { title: 'Main Agent', status: 'running' } };

		const agentIdx = messages.length;
		messages = [
			...messages,
			{
				role: 'agent',
				content: '',
				blocks: []
			}
		];

		try {
			const res = await fetch('/api/chat', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ message: payload, sessionId })
			});

			if (!res.body) throw new Error('No response body');

			const reader = res.body.getReader();
			const decoder = new TextDecoder();
			let buffer = '';

			while (true) {
				const { done, value } = await reader.read();
				if (done) {
					if (buffer.trim()) processLines(buffer);
					break;
				}
				buffer += decoder.decode(value, { stream: true });
				const parts = buffer.split('\n\n');
				buffer = parts.pop() || '';
				for (const part of parts) processLines(part);
			}

			function processLines(textBlock: string) {
				const lines = textBlock.split('\n');
				for (const line of lines) {
					if (!line.startsWith('data: ')) continue;
					const dataStr = line.slice(6).trim();
					if (!dataStr || dataStr === '[DONE]') continue;

					try {
						const event = JSON.parse(dataStr);
						if (event.type === 'system.session_created') {
							sessionId = event.sessionId;
							continue;
						}

						let msg = { ...messages[agentIdx] };
						const threadId = event.threadId || 'main';

						// Handle Turn and Thread Lifecycle
						if (event.type === 'turn.created') {
							msg.turnId = event.turnId;
						} else if (event.type === 'thread.created') {
							threadMeta[event.threadId] = { title: event.title || 'Subagent', status: 'running' };
						} else if (event.type === 'thread.done') {
							if (threadMeta[event.threadId]) threadMeta[event.threadId].status = 'done';

							// Iterate through all blocks and mark every block belonging to this thread as done
							for (const block of msg.blocks) {
								if (block.threadId === event.threadId) {
									block.status = 'done';
								}
							}
						}

						// Timeline Chunk Routing
						else if (event.type === 'model.message.delta' || event.type === 'tool.response') {
							let lastBlock = msg.blocks[msg.blocks.length - 1];

							// Splitting the timeline if the thread shifted
							if (!lastBlock || lastBlock.threadId !== threadId) {
								lastBlock = {
									id: Math.random().toString(36).slice(2),
									threadId: threadId,
									title:
										threadMeta[threadId]?.title ||
										(threadId === 'main' ? 'Main Agent' : 'Subagent'),
									isMain: threadId === 'main',
									status: threadMeta[threadId]?.status || 'running',
									subMessages: []
								};
								msg.blocks.push(lastBlock);
							}

							// Merging message text and tools
							if (event.type === 'model.message.delta') {
								let sub = lastBlock.subMessages.find((sm) => sm.id === event.id);
								if (!sub) {
									sub = { id: event.id, content: '', tools: [] };
									lastBlock.subMessages.push(sub);
								}

								if (event.content) sub.content += event.content;

								if (event.toolCalls) {
									for (const tc of event.toolCalls) {
										let tool = sub.tools.find((t) => t.index === tc.index);
										if (!tool) {
											tool = {
												index: tc.index,
												id: tc.id || '',
												name: tc.function?.name || 'unknown',
												args: '',
												result: '',
												status: 'streaming'
											};
											sub.tools.push(tool);
										}
										if (tc.id) tool.id = tc.id;
										if (tc.function?.name) tool.name = tc.function.name;
										if (tc.function?.arguments) tool.args += tc.function.arguments;
									}
								}
							}
							// Resolving tools by scanning backward through blocks
							else if (event.type === 'tool.response') {
								for (let i = msg.blocks.length - 1; i >= 0; i--) {
									const tool = msg.blocks[i].subMessages
										.flatMap((sm) => sm.tools)
										.find((t) => t.id === event.toolCallId);
									if (tool) {
										tool.result = event.content;
										tool.status = 'done';
										break;
									}
								}
							}
						}

						messages[agentIdx] = msg;
					} catch (e) {
						console.warn('Skipped malformed JSON:', dataStr);
					}
				}
			}
		} catch (error) {
			console.error('Chat error:', error);
			messages[agentIdx].blocks = [
				{
					id: 'error',
					threadId: 'main',
					title: 'Error',
					isMain: true,
					status: 'error',
					subMessages: [{ id: 'err', content: 'Connection lost.', tools: [] }]
				}
			];
		} finally {
			isThinking = false;
		}
	}
</script>

<!-- The HTML template remains exactly the same -->
<div class="flex h-screen flex-col bg-gray-900 p-4 font-sans text-gray-100">
	<h1 class="mb-6 text-3xl font-bold tracking-wider text-red-500">Double-O-Harness</h1>

	<!-- Chat History -->
	<div
		class="mb-4 flex-1 overflow-y-auto rounded-lg border border-gray-700 bg-gray-950 p-4 shadow-inner"
	>
		{#each messages as msg}
			<div
				class="mb-6 max-w-[85%] rounded-md p-4 {msg.role === 'user'
					? 'ml-auto bg-blue-900'
					: 'mr-auto'}"
			>
				{#if msg.role === 'user'}
					<!-- User Message -->
					<div class="whitespace-pre-wrap">{msg.content}</div>
				{:else}
					<!-- Agent Message (Thread Hierarchy) -->
					<div class="flex flex-col gap-4">
						{#each msg.blocks as block}
							<!-- Thread Wrapper: Conditionally apply purple styling if it's a subagent -->
							<div
								class={!block.isMain
									? 'mt-2 ml-4 rounded-lg border border-purple-500/50 bg-purple-950/20 p-4 shadow-sm'
									: ''}
							>
								{#if !block.isMain}
									<div class="mb-3 flex items-center gap-2 text-sm font-bold text-purple-400">
										<span>{block.status === 'running' ? '⚡' : '🏁'}</span>
										Subagent: {block.title}
									</div>
								{/if}

								<!-- Render the chunks for this thread. -->
								{#each block.subMessages as sub}
									<!-- Text & Intercepted Artifacts -->
									{#if sub.content}
										{#each parseContentBlocks(sub.content) as block}
											{#if block.type === 'text'}
												<div class="whitespace-pre-wrap text-gray-200">{block.content}</div>
											{:else if block.type === 'artifacts'}
												<!-- Download Links -->
												<div
													class="my-4 overflow-hidden rounded-md border border-indigo-500/30 bg-[#1e1e2e]"
												>
													<div
														class="flex items-center gap-2 bg-indigo-900/40 px-4 py-2 text-xs font-semibold text-indigo-300"
													>
														<span class="text-lg">📁</span>
														{block.items.length} file{block.items.length > 1 ? 's' : ''} generated
													</div>
													<div class="flex flex-col gap-1 p-2">
														{#each block.items as file}
															<a
																href={`/api/download?sessionId=${sessionId}&turnId=${msg.turnId}&path=${encodeURIComponent(file.path)}`}
																target="_blank"
																class="flex items-center justify-between rounded bg-gray-800 px-3 py-2 text-sm text-blue-400 transition-colors hover:bg-gray-700 hover:text-blue-300"
															>
																<span class="flex items-center gap-2">📄 {file.name}</span>
																<span>⬇️</span>
															</a>
														{/each}
													</div>
												</div>
											{/if}
										{/each}
									{/if}

									<!-- Tool Calls -->
									{#if sub.tools.length > 0}
										<div class="mt-3 rounded-md border border-gray-700 bg-gray-900 p-3 shadow-sm">
											<div class="mb-3 text-xs font-semibold tracking-wide text-gray-400 uppercase">
												Tools • {sub.tools.length} call{sub.tools.length > 1 ? 's' : ''}
											</div>
											<div class="space-y-2">
												{#each sub.tools as tool}
													<details
														class="group overflow-hidden rounded border border-gray-700 bg-gray-950"
													>
														<summary
															class="flex cursor-pointer items-center gap-2 p-2 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800"
														>
															<span class="text-green-500"
																>{tool.status === 'streaming' ? '🔄' : '✅'}</span
															>
															call_tool: {tool.name}
														</summary>
														<div
															class="border-t border-gray-700 p-3 font-mono text-xs text-gray-300"
														>
															<!-- Request -->
															<div class="mb-3 rounded border border-gray-800 bg-gray-900 p-2">
																<div class="mb-1 text-blue-400">Request</div>
																<pre class="whitespace-pre-wrap text-yellow-300">{formatJson(
																		tool.args
																	)}</pre>
															</div>
															<!-- Response -->
															{#if tool.result}
																<div class="rounded border border-gray-800 bg-gray-900 p-2">
																	<div class="mb-1 text-purple-400">Response</div>
																	<pre
																		class="max-h-48 overflow-y-auto whitespace-pre-wrap text-green-300">{formatJson(
																			tool.result
																		)}</pre>
																</div>
															{/if}
														</div>
													</details>
												{/each}
											</div>
										</div>
									{/if}
								{/each}
							</div>
						{/each}
					</div>
				{/if}
			</div>
		{/each}
		{#if isThinking}
			<div class="animate-pulse text-gray-500 italic">Agent is thinking...</div>
		{/if}
	</div>

	<!-- Input Box -->
	<div class="flex gap-3">
		<input
			type="text"
			bind:value={currentInput}
			onkeydown={(e) => e.key === 'Enter' && sendMessage()}
			disabled={isThinking}
			class="flex-1 rounded-lg border border-gray-600 bg-gray-800 p-3 text-white transition-colors focus:border-red-500 focus:outline-none disabled:opacity-50"
			placeholder="Ask the agent something..."
		/>
		<button
			onclick={sendMessage}
			disabled={isThinking}
			class="rounded-lg bg-red-600 px-6 py-3 font-bold shadow-md transition-colors hover:bg-red-500 disabled:opacity-50"
		>
			Execute
		</button>
	</div>
</div>
