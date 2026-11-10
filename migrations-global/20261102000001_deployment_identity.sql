-- 部署身份是持久化的随机标识，不是域名、端口或主机名。
--
-- Computer Use 的严格 principal 必须能区分「同一个用户在不同部署上的账号」。
-- 域名可以迁移、反代 Host 可以由调用方伪造，主机名也会在备份恢复或容器重建
-- 后改变；它们只能帮助客户端找到服务，不能参与身份判断。
--
-- 这一行随数据库备份恢复，因此恢复后的地址或机器名变化不会把既有授权
-- 误认成另一套部署。克隆数据库后若要成为独立部署，必须显式重建这一行；
-- 常规启动永远不能覆盖既有值。
CREATE TABLE deployment_identity (
    singleton     BOOLEAN     PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    deployment_id TEXT        NOT NULL UNIQUE
        CHECK (deployment_id ~ '^[0-9a-f]{64}$'),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

COMMENT ON TABLE deployment_identity IS
    '当前部署的持久随机身份；只允许一行。地址不是身份，deployment_id 才是。';
