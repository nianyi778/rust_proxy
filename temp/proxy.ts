import { Hono } from 'hono';
import type { Env } from '../types';
import { error } from '../utils/response';
import { rateLimit } from '../utils/rate-limit';
import { isAllowedOrigin, isBlockedProxyTargetWithDns, isPublicHttpUrl } from '../utils/security';

const app = new Hono<{ Bindings: Env }>();

// 检测是否为开发环境
const isDev = () => {
  // Wrangler 本地开发时没有 caches.default，或者可以通过环境变量判断
  return typeof caches === 'undefined' || !caches.default;
};

// 代理豆瓣图片，绕过防盗链
// 优化：使用 Cloudflare Cache API（免费）替代 KV 缓存
app.get('/image', async (c) => {
  const imageUrl = c.req.query('url');
  
  if (!imageUrl) {
    return c.json(error('Image URL is required'), 400);
  }
  if (imageUrl.length > 4000) {
    return c.json(error('Image URL is too long'), 400);
  }
  if (!isPublicHttpUrl(imageUrl)) {
    return c.json(error('Invalid URL'), 400);
  }

  // SSRF 防护：阻止访问私网/本机/元数据地址
  const block = await isBlockedProxyTargetWithDns(imageUrl);
  if (block.blocked) {
    return c.json(error('Blocked URL'), 403);
  }

  // 防滥用：若存在 Origin，则必须在允许列表中（注意：部分 <img>/<video> 跨域请求可能不带 Origin）
  if (!isDev()) {
    const origin = c.req.header('Origin');
    if (origin && !isAllowedOrigin(origin, c.env)) {
      return c.json(error('Forbidden'), 403);
    }
  }

  // 轻量限流（KV 未绑定时自动放行）
  // 高频接口：使用内存限流避免打爆 KV 写入额度
  const rl = await rateLimit(c, { limit: 300, window: 60, keyPrefix: 'rl:proxy:image', store: 'memory' });
  if (!rl.allowed) {
    return c.json(error('请求过于频繁，请稍后再试'), 429);
  }

  try {
    // 验证 URL 是否为豆瓣域名
    const url = new URL(imageUrl);
    const reqUrl = new URL(c.req.url);
    const hostHeader = c.req.header('host');
    // 避免自我代理造成递归（直接 host 相同，或参数里包含再次代理路径）
    const selfHost = url.hostname === reqUrl.hostname || (hostHeader && url.hostname === hostHeader);
    if (selfHost || url.pathname.startsWith('/api/proxy/image') || imageUrl.includes('/api/proxy/image')) {
      return c.json(error('Self-proxy is not allowed'), 400);
    }

    // 开发环境跳过缓存
    if (!isDev()) {
      // 使用 Cloudflare Cache API（免费，不消耗 KV 配额）
      const cache = caches.default;
      const cacheUrl = new URL(c.req.url);
      const cacheRequest = new Request(cacheUrl.toString());
      
      // 检查 Cache API 缓存
      const cachedResponse = await cache.match(cacheRequest);
      if (cachedResponse) {
        // 添加缓存命中标记
        const headers = new Headers(cachedResponse.headers);
        headers.set('X-Cache', 'HIT');
        return new Response(cachedResponse.body, {
          status: cachedResponse.status,
          headers,
        });
      }
    }

    // 获取图片，设置正确的 Referer
    const headers = new Headers({
      'User-Agent': 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36',
      'Accept': 'image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8',
    });

    if (url.hostname.includes('douban')) {
      headers.set('Referer', 'https://movie.douban.com/');
    }

    const response = await fetch(imageUrl, {
      headers,
    });

    if (!response.ok) {
      return c.json(error(`Failed to fetch image: ${response.status}`), 500);
    }

    const imageBuffer = await response.arrayBuffer();
    const contentType = response.headers.get('Content-Type') || 'image/jpeg';

    // 创建响应
    const imageResponse = new Response(imageBuffer, {
      headers: {
        'Content-Type': contentType,
        'Cache-Control': isDev() ? 'no-cache, no-store' : 'public, max-age=604800, stale-while-revalidate=86400',
        'X-Cache': 'MISS',
        'Vary': 'Accept-Encoding',
      },
    });

    // 生产环境存入 Cache API（免费，后台执行）
    if (!isDev()) {
      const cache = caches.default;
      const cacheUrl = new URL(c.req.url);
      const cacheRequest = new Request(cacheUrl.toString());
      c.executionCtx.waitUntil(cache.put(cacheRequest, imageResponse.clone()));
    }

    return imageResponse;
  } catch (err) {
    console.error('Image proxy error:', err);
    return c.json(error('Failed to proxy image'), 500);
  }
});

// ================== [新增] 视频流代理 API ==================
app.get('/stream', async (c) => {
  const url = c.req.query('url');
  
  if (!url) {
    return c.json(error('URL is required'), 400);
  }
  if (url.length > 6000) {
    return c.json(error('URL is too long'), 400);
  }
  if (!isPublicHttpUrl(url)) {
    return c.json(error('Invalid URL'), 400);
  }

  // SSRF 防护：阻止访问私网/本机/元数据地址
  const block = await isBlockedProxyTargetWithDns(url);
  if (block.blocked) {
    return c.json(error('Blocked URL'), 403);
  }

  // 防滥用：若存在 Origin，则必须在允许列表中（注意：部分媒体请求可能不带 Origin）
  if (!isDev()) {
    const origin = c.req.header('Origin');
    if (origin && !isAllowedOrigin(origin, c.env)) {
      return c.json(error('Forbidden'), 403);
    }
  }

  // 限流（KV 未绑定时自动放行；尽量宽松避免影响 HLS 分片）
  // 高频接口：使用内存限流避免打爆 KV 写入额度
  const rl = await rateLimit(c, { limit: 3000, window: 60, keyPrefix: 'rl:proxy:stream', store: 'memory' });
  if (!rl.allowed) {
    return c.json(error('请求过于频繁，请稍后再试'), 429);
  }

  try {
    const fetchHeaders = new Headers();
    
    // 1. 设置通用的 User-Agent，防止被简单的 UA 拦截
    fetchHeaders.set('User-Agent', 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36');
    
    // 2. 尝试伪造 Referer/Origin 以绕过防盗链
    // 注意：有些流媒体服务器可能会校验 Referer，这里尝试设为 URL 的源
    try {
      const u = new URL(url);
      fetchHeaders.set('Referer', u.origin + '/');
      fetchHeaders.set('Origin', u.origin);
    } catch (e) {
      // 忽略 URL 解析错误
    }

    // 3. [关键] 转发 Range 头
    // 浏览器播放视频（尤其是 mp4）时会发送 Range 请求部分内容，必须转发，否则无法拖拽或播放
    const range = c.req.header('Range');
    if (range) {
      fetchHeaders.set('Range', range);
    }

    // 4. 发起请求（设置超时，避免长时间等待）
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 15000); // 15秒超时
    
    let response: Response;
    try {
      response = await fetch(url, {
        method: c.req.method, // 支持 GET 和 HEAD
        headers: fetchHeaders,
        redirect: 'follow',
        signal: controller.signal,
      });
    } catch (fetchErr) {
      clearTimeout(timeoutId);
      if (fetchErr instanceof Error && fetchErr.name === 'AbortError') {
        console.error(`[Proxy] Timeout fetching: ${url}`);
        return c.json(error('源服务器响应超时'), 504);
      }
      throw fetchErr;
    }
    clearTimeout(timeoutId);

    // 4.1 如果是 403 Forbidden，尝试移除 Referer/Origin 重试
    // 有些源（如某些图床或 CDN）如果 Referer 不匹配会拒绝，但没有 Referer 反而允许
    if (response.status === 403) {
      console.log(`[Proxy] 403 with Referer, retrying without Referer: ${url}`);
      fetchHeaders.delete('Referer');
      fetchHeaders.delete('Origin');
      
      const retryController = new AbortController();
      const retryTimeoutId = setTimeout(() => retryController.abort(), 10000);
      try {
        response = await fetch(url, {
          method: c.req.method,
          headers: fetchHeaders,
          redirect: 'follow',
          signal: retryController.signal,
        });
      } catch (retryErr) {
        clearTimeout(retryTimeoutId);
        if (retryErr instanceof Error && retryErr.name === 'AbortError') {
          return c.json(error('源服务器响应超时'), 504);
        }
        throw retryErr;
      }
      clearTimeout(retryTimeoutId);
    }

    // 4.2 如果上游返回错误状态码，直接返回友好提示
    if (!response.ok && response.status !== 206) { // 206 是 Range 请求的正常响应
      console.error(`[Proxy] Upstream error ${response.status}: ${url}`);
      return c.json(error(`源服务器返回错误: ${response.status}`), 502);
    }

    // [新增] M3U8 重写逻辑
    // 如果是 m3u8 文件，需要重写内部的 URL，使其也经过代理
    const contentType = response.headers.get('Content-Type') || '';
    const urlLower = url.toLowerCase();
    
    if ((contentType.includes('application/vnd.apple.mpegurl') || 
         contentType.includes('application/x-mpegURL') ||
         urlLower.includes('.m3u8')) && 
         response.ok) {
      
      const m3u8Content = await response.text();
      const reqUrl = new URL(c.req.url);
      const proxyOrigin = reqUrl.origin;
      
      const rewrittenContent = rewriteM3U8(m3u8Content, url, proxyOrigin);
      
      const newHeaders = new Headers(response.headers);
      newHeaders.set('Access-Control-Allow-Origin', '*');
      newHeaders.set('Access-Control-Allow-Methods', 'GET, HEAD, OPTIONS');
      newHeaders.set('Content-Type', 'application/vnd.apple.mpegurl');
      // m3u8 清单文件：短缓存
      newHeaders.set('Cache-Control', 'public, max-age=10, stale-while-revalidate=30');
      
      return new Response(rewrittenContent, {
        status: 200,
        headers: newHeaders,
      });
    }

    // 5. 构造响应头
    // 复制原始响应头，但要注意 CORS 和某些 Cloudflare 特有头的处理
    const newHeaders = new Headers(response.headers);
    
    // 强制允许跨域 (Hono 的 cors 中间件可能不会覆盖 Response 对象上的头)
    newHeaders.set('Access-Control-Allow-Origin', '*');
    newHeaders.set('Access-Control-Allow-Methods', 'GET, HEAD, OPTIONS');
    newHeaders.set('Access-Control-Expose-Headers', 'Content-Length, Content-Range, Content-Type, Accept-Ranges');

    // 针对不同类型设置缓存策略
    if (urlLower.includes('.ts') || urlLower.includes('.m4s')) {
      // TS/M4S 分片：长缓存，内容不变
      newHeaders.set('Cache-Control', 'public, max-age=86400, immutable');
    } else if (urlLower.includes('.mp4') || urlLower.includes('.mkv')) {
      // 完整视频文件：长缓存
      newHeaders.set('Cache-Control', 'public, max-age=3600');
    }

    // 6. 返回流式响应
    return new Response(response.body, {
      status: response.status,
      statusText: response.statusText,
      headers: newHeaders,
    });

  } catch (err) {
    console.error('Stream proxy error:', err);
    return c.json(error('Failed to proxy stream'), 500);
  }
});

/**
 * 重写 m3u8 文件内容
 * 将所有资源 URL 替换为代理 URL
 */
function rewriteM3U8(content: string, baseUrl: string, proxyOrigin: string): string {
  const lines = content.split('\n');
  const baseUrlObj = new URL(baseUrl);
  const baseDir = baseUrl.substring(0, baseUrl.lastIndexOf('/') + 1);
  
  // 辅助函数：将相对URL转换为绝对URL
  const resolveUrl = (url: string): string => {
    if (url.startsWith('http://') || url.startsWith('https://')) {
      return url;
    }
    if (url.startsWith('/')) {
      return `${baseUrlObj.protocol}//${baseUrlObj.host}${url}`;
    }
    return baseDir + url;
  };
  
  const rewrittenLines = lines.map(line => {
    // 处理 #EXT-X-KEY 标签中的 URI
    if (line.startsWith('#EXT-X-KEY:')) {
      // 匹配 URI="..." 或 URI='...' 或 URI=...
      const uriMatch = line.match(/URI=["']?([^"',]+)["']?/);
      if (uriMatch && uriMatch[1]) {
        const originalUri = uriMatch[1];
        const absoluteUri = resolveUrl(originalUri);
        // 替换原始URI为代理URI
        const proxiedUri = `${proxyOrigin}/api/proxy/stream?url=${encodeURIComponent(absoluteUri)}`;
        return line.replace(/URI=["']?[^"',]+["']?/, `URI="${proxiedUri}"`);
      }
      return line;
    }
    
    // 跳过其他注释行和空行
    if (line.startsWith('#') || line.trim() === '') {
      return line;
    }
    
    // 处理资源 URL（.ts 片段等）
    const resourceUrl = resolveUrl(line.trim());
    const proxiedUrl = `${proxyOrigin}/api/proxy/stream?url=${encodeURIComponent(resourceUrl)}`;
    return proxiedUrl;
  });
  
  return rewrittenLines.join('\n');
}

export default app;
