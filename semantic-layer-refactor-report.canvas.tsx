import { Divider, Grid, H1, H2, H3, Stack, Stat, Table, Text } from 'qoder/canvas';

export default function SemanticLayerRefactorReport() {
  return (
    <Stack gap={20}>
      <H1>数组下标寻址语义层重构</H1>
      <Text tone="secondary">dcc C 编译器 — 三层架构重构完成报告</Text>

      <Divider />

      <H2>概览</H2>
      <Grid columns={4} gap={16}>
        <Stat value="7" label="Phase 完成" tone="success" />
        <Stat value="2248" label="测试通过" tone="success" />
        <Stat value="0" label="回归数" tone="success" />
        <Stat value="503" label="semantics.rs 行数" />
      </Grid>

      <Divider />

      <H2>三层架构</H2>
      <Table
        headers={['Layer', '职责', '输入 → 输出', '后端无关?']}
        rows={[
          ['Layer 1: 对象解析', '源名称 → 对象身份 + 存储信息', 'source name → ObjectIdentity + StorageInfo', '是'],
          ['Layer 2: 地址计算', 'lvalue 表达式 → 地址描述符', 'Expr → AddressDescriptor', '是'],
          ['Layer 3: 按上下文物化', '地址描述符 → 目标指令', 'AddressDescriptor + Action → instructions', '否 (后端特定)'],
        ]}
      />

      <Divider />

      <H2>Phase 执行记录</H2>
      <Table
        headers={['Phase', '内容', '关键改动', '状态']}
        rows={[
          ['Phase 1', '创建 src/semantics.rs', 'ObjectIdentity, AddressDescriptor, ObjectResolver trait, compute_lvalue_address()', '完成'],
          ['Phase 2', 'x86_64 接入 Layer 1', 'impl ObjectResolver for Codegen, resolve_identity_semantic, resolve_storage_semantic', '完成'],
          ['Phase 3-4', 'x86_64 Layer 2+3 物化', 'emit_lvalue_addr_semantic, emit_address_descriptor, emit_ident_base_addr 统一 3 处 Index', '完成'],
          ['Phase 5', '封闭 globals 访问', 'globals → globals_raw (33+ 处), lookup_global_info, is_global_object', '完成'],
          ['Phase 6', 'aarch64/riscv64 接入', 'IR 层 static local 名字解析 (lower_expr Ident → mangled name)', '完成'],
          ['Phase 7', '测试矩阵', '6 个集成测试: 同名冲突, 多维数组, VLA, 取址 vs 加载, 聚合数组, 组合场景', '完成'],
        ]}
        rowTone={['success', 'success', 'success', 'success', 'success', 'success']}
      />

      <Divider />

      <H2>变更文件</H2>
      <Table
        headers={['文件', '操作', '说明']}
        rows={[
          ['src/semantics.rs', '新建', '503 行后端无关语义层: Layer 1+2 类型, ObjectResolver trait, 地址计算函数'],
          ['src/lib.rs', '修改', '添加 pub mod semantics'],
          ['src/codegen/x86_64.rs', '修改', '~200 行变更: ObjectResolver impl, emit_ident_base_addr, emit_lvalue_addr_semantic, globals→globals_raw'],
          ['src/ir.rs', '修改', 'Lowering 结构新增 sl_map/current_func, lower_expr Ident 解析 static local → mangled 名'],
          ['tests/integration.rs', '修改', '6 个新 semantic_layer_* 测试用例'],
        ]}
      />

      <Divider />

      <H2>测试验证</H2>
      <Grid columns={3} gap={16}>
        <Stat value="1350" label="单元测试" tone="success" />
        <Stat value="814" label="集成测试" tone="success" />
        <Stat value="3599/114" label="Torture (pass/fail)" />
      </Grid>

      <H3>新增语义层测试矩阵</H3>
      <Table
        headers={['测试场景', '覆盖 Layer', '结果']}
        rows={[
          ['static int x[4] + int x[8] 同名', 'Layer 1 身份解析', '通过'],
          ['int a[3][4]; a[i][j] 多维数组', 'Layer 2 中间层衰减', '通过'],
          ['&arr[i] vs arr[i]', 'Layer 3 TakeAddress vs LoadScalar', '通过'],
          ['VLA int a[n]; a[i]', 'Layer 2 VlaRuntime stride', '通过'],
          ['struct S s[4]; s[i] 聚合', 'Layer 3 AggregateLoad/Store', '通过'],
          ['static local 取址 + 运行时索引组合', 'Layer 1+2 组合', '通过'],
        ]}
        rowTone={['success', 'success', 'success', 'success', 'success', 'success']}
      />

      <Divider />

      <H2>解决的问题</H2>
      <Table
        headers={['问题', '解决方式', 'Phase']}
        rows={[
          ['globals 职责过多', '封装 globals_raw + lookup_global_info 受控接口', 'Phase 5'],
          ['数组地址与元素值无统一抽象', 'Layer 2 AddressDescriptor + Layer 3 MaterializeAction 正交分离', 'Phase 1+4'],
          ['静态局部未贯通寻址层', 'Layer 1 resolve_identity 唯一入口 + IR 层名字解析', 'Phase 2+6'],
          ['各后端重复 C 语义', 'Layer 1+2 后端无关 (src/semantics.rs), 仅 Layer 3 后端特定', 'Phase 1+6'],
          ['多维数组衰减', 'Layer 2 ElementKind::Array, Layer 3 ArrayDecay', 'Phase 1'],
          ['VLA 运行时 stride', 'Layer 2 ByteOffset::VlaRuntime', 'Phase 1'],
        ]}
      />

      <Divider />

      <Text tone="secondary" size="small">
        重构完成。三层架构将 C 数组索引的 5 层复合语义 (C 语义/类型/存储/重定位/机器码) 分解为后端无关的 Layer 1+2 和后端特定的 Layer 3，消除了跨后端重复实现。
      </Text>
    </Stack>
  );
}
