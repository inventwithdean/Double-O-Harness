<script lang="ts">
	import { tick } from 'svelte';
	import { marked } from 'marked';
	import DOMPurify from 'isomorphic-dompurify';

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

	type PendingQuestion = {
		threadId: string;
		toolCallId: string;
		question: string;
		options: string[];
	};

	let sessionId = $state('');
	let messages = $state<Message[]>([]);
	let currentInput = $state('');
	let isThinking = $state(false);

	let pendingQuestion = $state<PendingQuestion | null>(null);
	let customAnswer = $state('');

	// Scroll Management
	let chatContainer = $state<HTMLDivElement | null>(null);
	let isScrolledUp = $state(false);

	let threadMeta = $state<Record<string, { title: string; status: 'running' | 'done' }>>({});

	function handleScroll() {
		if (!chatContainer) return;
		const { scrollTop, scrollHeight, clientHeight } = chatContainer;
		// If the user scrolls up more than 50px from the bottom, they are reading history
		isScrolledUp = scrollHeight - scrollTop - clientHeight > 50;
	}

	async function scrollToBottom() {
		if (!chatContainer || isScrolledUp) return;
		await tick();
		chatContainer.scrollTo({
			top: chatContainer.scrollHeight,
			behavior: 'smooth'
		});
	}

	function formatJson(raw: string) {
		if (!raw) return '{}';
		try {
			return JSON.stringify(JSON.parse(raw), null, 2);
		} catch (e) {
			return raw;
		}
	}

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
		messages = [...messages, { role: 'user', content: payload, blocks: [] }];
		currentInput = '';

		await triggerTurn({ message: payload });
	}

	async function sendToolResponse(content: string) {
		if (!pendingQuestion || !content.trim()) return;

		const payload = {
			type: 'user.tool_response',
			threadId: pendingQuestion.threadId,
			toolCallId: pendingQuestion.toolCallId,
			content
		};

		// Visually add the user's choice to the chat
		messages = [...messages, { role: 'user', content, blocks: [] }];

		for (const msg of messages) {
			for (const block of msg.blocks) {
				for (const sub of block.subMessages) {
					const tool = sub.tools.find((t) => t.id === pendingQuestion?.toolCallId);
					if (tool) {
						tool.status = 'done';
						tool.result = JSON.stringify({ user_response: content }, null, 2);
					}
				}
			}
		}

		// Reset state
		pendingQuestion = null;
		customAnswer = '';

		await triggerTurn({ toolResponse: payload });
	}

	async function triggerTurn(requestBody: any) {
		isThinking = true;
		isScrolledUp = false;
		scrollToBottom();

		threadMeta = { main: { title: 'Main Agent', status: 'running' } };
		const agentIdx = messages.length;
		messages = [...messages, { role: 'agent', content: '', blocks: [] }];

		try {
			const res = await fetch('/api/chat', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ ...requestBody, sessionId })
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

						// Intercept tool.response_required
						if (event.type === 'tool.response_required') {
							for (const ref of event.toolCalls) {
								let foundTool;
								// Scan existing blocks to find the tool call details
								for (const block of messages[agentIdx].blocks) {
									for (const sub of block.subMessages) {
										foundTool = sub.tools.find((t) => t.id === ref.id);
										if (foundTool) break;
									}
									if (foundTool) break;
								}

								if (foundTool && foundTool.name === 'ask_user_question') {
									try {
										const args = JSON.parse(foundTool.args || '{}');
										pendingQuestion = {
											threadId: event.threadId,
											toolCallId: ref.id,
											question: args.question || 'Please select an option or provide details:',
											options: args.options || []
										};
										scrollToBottom();
									} catch (e) {
										console.warn('Failed to parse question arguments');
									}
								}
							}
							continue; // Skip normal block processing for this event
						}

						if (event.type === 'system.session_created') {
							sessionId = event.sessionId;
							continue;
						}

						let msg = { ...messages[agentIdx] };
						const threadId = event.threadId || 'main';

						if (event.type === 'turn.created') {
							msg.turnId = event.turnId;
						} else if (event.type === 'thread.created') {
							threadMeta[event.threadId] = { title: event.title || 'Subagent', status: 'running' };
						} else if (event.type === 'thread.done') {
							if (threadMeta[event.threadId]) threadMeta[event.threadId].status = 'done';
							for (const block of msg.blocks) {
								if (block.threadId === event.threadId) {
									block.status = 'done';
								}
							}
						} else if (event.type === 'model.message.delta' || event.type === 'tool.response') {
							let lastBlock = msg.blocks[msg.blocks.length - 1];
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
							} else if (event.type === 'tool.response') {
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
						scrollToBottom();
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
			scrollToBottom();
		} finally {
			isThinking = false;
		}
	}
</script>

<div
	class="flex h-screen w-full flex-col items-center overflow-hidden bg-slate-50 font-sans text-slate-800"
>
	<div class="flex h-full w-full max-w-4xl flex-col p-4 md:p-6 lg:px-8 lg:py-6">
		<!-- Header -->
		<header class="mb-5 flex shrink-0 items-center justify-between pl-2">
			<h1 class="text-xl font-bold tracking-tight text-slate-900">
				Double-O<span class="font-medium text-slate-400">-Harness</span>
			</h1>
			<div
				class="flex items-center gap-2 rounded-full border border-slate-200 bg-white px-3 py-1 text-xs font-semibold tracking-wider text-slate-500 uppercase shadow-sm"
			>
				<span class="relative flex h-2 w-2">
					<span
						class="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75"
					></span>
					<span class="relative inline-flex h-2 w-2 rounded-full bg-emerald-500"></span>
				</span>
				Active
			</div>
		</header>

		<!-- Chat History -->
		<div
			bind:this={chatContainer}
			onscroll={handleScroll}
			class="thin-scrollbar flex-1 overflow-x-hidden overflow-y-auto rounded-2xl border border-slate-200 bg-white shadow-sm"
		>
			<div class="flex flex-col gap-6 p-4 md:p-6">
				{#if messages.length === 0}
					<div class="flex h-full flex-col items-center justify-center text-slate-400">
						<svg
							class="mb-4 h-12 w-12 opacity-20"
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="1.5"
							><path
								stroke-linecap="round"
								stroke-linejoin="round"
								d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5"
							/></svg
						>
						<p class="text-sm">Awaiting initial directive.</p>
					</div>
				{/if}

				{#each messages as msg}
					<div class="flex w-full {msg.role === 'user' ? 'justify-end' : 'justify-start'}">
						<div class="flex max-w-[95%] min-w-0 flex-col md:max-w-[85%]">
							{#if msg.role === 'user'}
								<div
									class="rounded-2xl rounded-br-sm border border-slate-800 bg-slate-900 p-4 text-[15px] leading-relaxed break-words whitespace-pre-wrap text-white shadow-md"
								>
									{msg.content}
								</div>
							{:else}
								<div class="flex min-w-0 flex-col gap-4">
									{#each msg.blocks as block}
										<div
											class={!block.isMain
												? 'mt-2 min-w-0 rounded-r-xl border-l-[3px] border-indigo-400 bg-gradient-to-r from-indigo-50/50 to-transparent py-3 pr-2 pl-4'
												: 'flex min-w-0 flex-col gap-3'}
										>
											{#if !block.isMain}
												<div
													class="mb-2 flex items-center gap-2 text-xs font-bold tracking-wider text-indigo-500 uppercase"
												>
													{#if block.status === 'running'}
														<svg
															class="h-3.5 w-3.5 animate-spin text-indigo-400"
															viewBox="0 0 24 24"
															fill="none"
															><circle
																cx="12"
																cy="12"
																r="10"
																stroke="currentColor"
																stroke-width="4"
																stroke-dasharray="32"
																stroke-dashoffset="32"
																stroke-linecap="round"
															/></svg
														>
													{:else}
														<svg
															class="h-3.5 w-3.5"
															fill="none"
															viewBox="0 0 24 24"
															stroke="currentColor"
															><path
																stroke-linecap="round"
																stroke-linejoin="round"
																stroke-width="2.5"
																d="M5 13l4 4L19 7"
															/></svg
														>
													{/if}
													{block.title}
												</div>
											{/if}

											<div class="flex min-w-0 flex-col gap-3">
												{#each block.subMessages as sub}
													{#if sub.content}
														{#each parseContentBlocks(sub.content) as contentBlock}
															{#if contentBlock.type === 'text'}
																<!-- Rendered Markdown with Tailwind Prose -->
																<div
																	class="prose prose-sm max-w-none text-[15px] leading-relaxed break-words prose-slate"
																>
																	{@html DOMPurify.sanitize(marked.parse(contentBlock.content))}
																</div>
															{:else if contentBlock.type === 'artifacts'}
																<!-- Artifact Cards -->
																<div
																	class="my-2 overflow-hidden rounded-xl border border-slate-200 bg-white shadow-sm"
																>
																	<div
																		class="flex items-center gap-2 border-b border-slate-100 bg-slate-50 px-4 py-2.5 text-[11px] font-bold tracking-wider text-slate-500 uppercase"
																	>
																		<svg
																			class="h-4 w-4 text-slate-400"
																			viewBox="0 0 24 24"
																			fill="none"
																			stroke="currentColor"
																			><path
																				stroke-linecap="round"
																				stroke-linejoin="round"
																				stroke-width="2"
																				d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z"
																			/></svg
																		>
																		{contentBlock.items.length} Artifact{contentBlock.items.length >
																		1
																			? 's'
																			: ''}
																	</div>
																	<div class="flex flex-col p-1.5">
																		{#each contentBlock.items as file}
																			<a
																				href={`/api/download?sessionId=${sessionId}&turnId=${msg.turnId}&path=${encodeURIComponent(file.path)}`}
																				target="_blank"
																				class="group flex items-center justify-between rounded-lg px-3 py-2.5 text-sm transition-colors hover:bg-slate-50 hover:text-slate-900"
																			>
																				<span
																					class="flex items-center gap-2.5 truncate font-medium text-slate-700 group-hover:text-slate-900"
																				>
																					<svg
																						class="h-4 w-4 shrink-0 text-indigo-400"
																						viewBox="0 0 24 24"
																						fill="none"
																						stroke="currentColor"
																						><polyline
																							stroke-linecap="round"
																							stroke-linejoin="round"
																							stroke-width="2"
																							points="4 17 10 11 4 5"
																						/><line
																							stroke-linecap="round"
																							stroke-linejoin="round"
																							stroke-width="2"
																							x1="12"
																							y1="19"
																							x2="20"
																							y2="19"
																						/></svg
																					>
																					{file.name}
																				</span>
																				<svg
																					class="h-4 w-4 shrink-0 text-slate-300 transition-colors group-hover:text-indigo-500"
																					viewBox="0 0 24 24"
																					fill="none"
																					stroke="currentColor"
																					><path
																						stroke-linecap="round"
																						stroke-linejoin="round"
																						stroke-width="2"
																						d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4"
																					/></svg
																				>
																			</a>
																		{/each}
																	</div>
																</div>
															{/if}
														{/each}
													{/if}

													{#if sub.tools.length > 0}
														<div
															class="mt-1 min-w-0 rounded-xl border border-slate-200 bg-slate-50 p-2 shadow-sm"
														>
															<div
																class="mb-2 px-2 pt-1 text-[10px] font-bold tracking-wider text-slate-400 uppercase"
															>
																System Operations &bull; {sub.tools.length} call{sub.tools.length >
																1
																	? 's'
																	: ''}
															</div>
															<div class="min-w-0 space-y-1.5">
																{#each sub.tools as tool}
																	<details
																		class="group min-w-0 overflow-hidden rounded-lg border border-slate-200 bg-white transition-all open:border-slate-300 open:shadow-sm"
																	>
																		<summary
																			class="flex cursor-pointer items-center justify-between px-3 py-2 text-xs font-medium hover:bg-slate-50 focus:outline-none"
																		>
																			<div class="flex items-center gap-2.5 truncate">
																				{#if tool.status === 'streaming'}
																					<svg
																						class="h-3.5 w-3.5 shrink-0 animate-spin text-slate-400"
																						viewBox="0 0 24 24"
																						fill="none"
																						><circle
																							cx="12"
																							cy="12"
																							r="10"
																							stroke="currentColor"
																							stroke-width="4"
																							stroke-dasharray="32"
																							stroke-dashoffset="32"
																							stroke-linecap="round"
																						/></svg
																					>
																				{:else}
																					<svg
																						class="h-3.5 w-3.5 shrink-0 text-emerald-500"
																						fill="none"
																						viewBox="0 0 24 24"
																						stroke="currentColor"
																						><path
																							stroke-linecap="round"
																							stroke-linejoin="round"
																							stroke-width="2.5"
																							d="M5 13l4 4L19 7"
																						/></svg
																					>
																				{/if}
																				<span
																					class="truncate font-mono text-[11.5px] text-indigo-600"
																					>{tool.name}</span
																				>
																			</div>
																			<svg
																				class="h-4 w-4 shrink-0 text-slate-400 transition-transform group-open:rotate-180"
																				viewBox="0 0 24 24"
																				fill="none"
																				stroke="currentColor"
																				><polyline
																					stroke-linecap="round"
																					stroke-linejoin="round"
																					stroke-width="2"
																					points="6 9 12 15 18 9"
																				/></svg
																			>
																		</summary>
																		<div
																			class="custom-scrollbar overflow-x-auto border-t border-slate-100 bg-[#0f172a] p-3 text-left"
																		>
																			<div class="mb-4">
																				<div
																					class="mb-1.5 text-[10px] font-bold tracking-wider text-slate-500 uppercase"
																				>
																					Request
																				</div>
																				<pre
																					class="custom-scrollbar w-fit min-w-full font-mono text-[11px] break-all whitespace-pre-wrap text-emerald-300">{formatJson(
																						tool.args
																					)}</pre>
																			</div>
																			{#if tool.result}
																				<div class="border-t border-slate-700/50 pt-3">
																					<div
																						class="mb-1.5 text-[10px] font-bold tracking-wider text-slate-500 uppercase"
																					>
																						Response
																					</div>
																					<pre
																						class="custom-scrollbar max-h-56 w-fit min-w-full overflow-y-auto font-mono text-[11px] break-all whitespace-pre-wrap text-slate-300">{formatJson(
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
										</div>
									{/each}
								</div>
							{/if}
						</div>
					</div>
				{/each}

				{#if isThinking}
					<div
						class="flex items-center gap-2 pl-2 text-xs font-semibold tracking-widest text-slate-400 uppercase"
					>
						<svg class="h-4 w-4 animate-spin text-slate-300" viewBox="0 0 24 24" fill="none"
							><circle
								cx="12"
								cy="12"
								r="10"
								stroke="currentColor"
								stroke-width="3"
								stroke-dasharray="32"
								stroke-dashoffset="32"
								stroke-linecap="round"
							/></svg
						>
						Processing...
					</div>
				{/if}
			</div>
		</div>

		<!-- Dynamic Input Area -->
		<div class="mt-5 shrink-0 pb-2">
			{#if pendingQuestion}
				<!-- Interactive Action Request Panel -->
				<div
					class="animate-in fade-in slide-in-from-bottom-2 flex flex-col gap-4 rounded-2xl border-2 border-indigo-200 bg-indigo-50/40 p-5 shadow-sm transition-all"
				>
					<div class="flex items-center gap-2.5 text-sm font-bold text-indigo-800">
						<svg
							class="h-5 w-5 text-indigo-500"
							fill="none"
							viewBox="0 0 24 24"
							stroke="currentColor"
						>
							<path
								stroke-linecap="round"
								stroke-linejoin="round"
								stroke-width="2"
								d="M8.228 9c.549-1.165 2.03-2 3.772-2 2.21 0 4 1.343 4 3 0 1.4-1.278 2.575-3.006 2.907-.542.104-.994.54-.994 1.093m0 3h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z"
							/>
						</svg>
						{pendingQuestion.question}
					</div>

					{#if pendingQuestion.options && pendingQuestion.options.length > 0}
						<div class="flex flex-wrap gap-2">
							{#each pendingQuestion.options as opt}
								<button
									onclick={() => sendToolResponse(opt)}
									disabled={isThinking}
									class="rounded-xl border border-indigo-200 bg-white px-4 py-2 text-[13px] font-medium text-slate-700 shadow-sm transition-all hover:border-indigo-300 hover:bg-indigo-50 hover:text-indigo-700 active:scale-95 disabled:opacity-50"
								>
									{opt}
								</button>
							{/each}
						</div>
					{/if}

					<div class="flex items-center gap-2">
						<input
							type="text"
							bind:value={customAnswer}
							onkeydown={(e) => e.key === 'Enter' && sendToolResponse(customAnswer)}
							disabled={isThinking}
							placeholder="Or type a custom response..."
							class="h-10 w-full rounded-xl border border-slate-200 bg-white px-3 text-sm text-slate-900 transition-all outline-none focus:border-indigo-400 focus:ring-4 focus:ring-indigo-500/10 disabled:opacity-50"
						/>
						<button
							onclick={() => sendToolResponse(customAnswer)}
							disabled={isThinking || !customAnswer.trim()}
							class="flex h-10 shrink-0 items-center justify-center rounded-xl bg-indigo-600 px-4 text-sm font-semibold text-white transition-all hover:bg-indigo-500 disabled:pointer-events-none disabled:opacity-30"
						>
							Send
						</button>
					</div>
				</div>
			{:else}
				<!-- Standard Input Box -->
				<div
					class="relative flex items-center gap-2 rounded-2xl border border-slate-200 bg-white p-1.5 shadow-sm transition-all focus-within:border-indigo-400 focus-within:ring-4 focus-within:ring-indigo-500/10 hover:border-slate-300"
				>
					<input
						type="text"
						bind:value={currentInput}
						onkeydown={(e) => e.key === 'Enter' && sendMessage()}
						disabled={isThinking}
						class="h-10 w-full bg-transparent px-3 text-[15px] text-slate-900 placeholder-slate-400 outline-none disabled:opacity-50"
						placeholder="Enter directive sequence..."
					/>
					<button
						onclick={sendMessage}
						disabled={isThinking || !currentInput.trim()}
						class="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-slate-900 text-white transition-all hover:bg-slate-700 disabled:pointer-events-none disabled:opacity-30"
					>
						<svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor"
							><path
								stroke-linecap="round"
								stroke-linejoin="round"
								stroke-width="2"
								d="M22 2L11 13M22 2l-7 20-4-9-9-4 20-7z"
							/></svg
						>
					</button>
				</div>
			{/if}
		</div>
	</div>
</div>

<style>
	/* Scoped global styles for the super thin, refined scrollbars */
	:global(.thin-scrollbar::-webkit-scrollbar),
	:global(.custom-scrollbar::-webkit-scrollbar) {
		width: 4px;
		height: 4px;
	}

	:global(.thin-scrollbar::-webkit-scrollbar-track),
	:global(.custom-scrollbar::-webkit-scrollbar-track) {
		background: transparent;
	}

	:global(.thin-scrollbar::-webkit-scrollbar-thumb),
	:global(.custom-scrollbar::-webkit-scrollbar-thumb) {
		background-color: #cbd5e1; /* Tailwind slate-300 */
		border-radius: 10px;
	}

	:global(.thin-scrollbar::-webkit-scrollbar-thumb:hover),
	:global(.custom-scrollbar::-webkit-scrollbar-thumb:hover) {
		background-color: #94a3b8; /* Tailwind slate-400 */
	}

	:global(pre.custom-scrollbar::-webkit-scrollbar-thumb) {
		background-color: #475569; /* slate-600 */
	}
	:global(pre.custom-scrollbar::-webkit-scrollbar-thumb:hover) {
		background-color: #64748b; /* slate-500 */
	}
</style>
