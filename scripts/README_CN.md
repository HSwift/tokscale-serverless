# Qoder 测量参考

[English](README.md)

`qoder_calibrate.py` 是可选的 Python 3 离线参考脚本，用于拟合你自己的模型分项单价。
代码不包含模型清单、实测系数、账号配置或会话样本，不发起网络请求，也不打包进采集器发布压缩包。

1. 为 Qoder CLI 进程设置 `QODER_EXPOSE_TOKEN_USAGE=1`，手动选择自己账号可用的固定模型。
   自行发起长短输入、长短输出，以及重复上下文的调用，覆盖冷输入和缓存命中。
   这些调用会消耗你的 credits；脚本不会自动调用模型。跳过 Auto 和其他动态路由别名。
2. 将对应会话的 JSONL 文件复制到本地 `train` 目录。通常来自
   `~/.qoder/projects/**/*.jsonl`（含 `subagents`）；修改过目录时使用自己的位置。
   每个模型至少收集五条完整请求，包含真实的输入、输出、缓存读取量及所选 credits 字段。
   再为每个模型新建会话，生成至少一条独立验证请求，复制到 `validate` 目录。
   两个目录放在本仓库外，不要把同一请求的副本分别作为训练和验证数据。
3. 在仓库根目录运行，路径替换成自己的目录：

   ```sh
   python3 scripts/qoder_calibrate.py \
     --train /path/to/train \
     --validate /path/to/validate \
     --credits-field original_credits \
     --output /path/to/qoder-coeffs.json
   ```

   Windows 可使用 `py -3` 和自己的路径。折前单价选择 `original_credits`，实扣单价选择
   `credits`。测量与后续估算记录都必须有选中的字段，不能互相替代。
4. 在本地检查生成结果，再自行复制到采集器的 `device.json` 同目录，或通过
   `TOKSCALE_QODER_COEFFS` 指定。脚本不会自动安装或覆盖文件；采集器下次扫描时重新加载。
   输出包含你实测的模型 ID，应保留在本地。

脚本仅读取每条 `type == "assistant"` 的 `message.usage`，按请求/消息 ID 去重，
忽略 `result` 中的会话累计值。模型名取 `message.model`，缺少时取 `usage.model`，
与采集器一致。没有真实 token、包含缓存写入、缺少所选 credits 字段的记录不参与拟合。
新输入为 `input_tokens - cache_read_input_tokens`，缓存只减一次。

至少需要三种独立的 token 组合才能区分三个单价。样本不足以区分单价、单价无效、
缺少独立验证样本，或任一训练/验证请求的 credits 误差超过 1% 时，该模型不写入配置。
脚本会显示通过的模型数量；缺失的模型需要增加不同类型的请求或重新选择测量时段。
不要混用不同定价/折扣时期的数据。窗口候选只从你自己的输入量和上下文比例测量，
代码中不附带各模型的窗口配置。

这里验证的是线性 **credits 单价模型**，不等于隐藏 token 后的估算误差。
缓存/输出拆分与窗口推断仍是近似值。定价或路由变化后应重新测量；采集器始终优先采用真实 token。

合成数据检查：`python3 -m unittest discover -s scripts -p 'test_*.py'`。
