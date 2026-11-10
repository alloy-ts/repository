

    docs.rs

    Rust

Skip to main content
Files
datafusion_distributed
codec
common
coordinator
distributed_planner
events
execution_plans
metrics
protocol
task_unit_feed
mod.rs
remote_task_unit_feed.rs
task_unit.rs
task_unit_feed.rs
task_unit_feed_provider.rs
task_unit_feed_registry.rs
task_scheduler
config_extension_ext.rs
distributed_ext.rs
explain_analyze.rs
lib.rs
passthrough_headers.rs
stage.rs
task_scheduler_resolver.rs
datafusion_distributed/task_unit_feed/
task_unit_feed_provider.rs
Search

use crate::TaskUnit;
use datafusion::common::Result;
use datafusion::execution::TaskContext;
use datafusion::physical_plan::metrics::ExecutionPlanMetricsSet;
use futures::stream::BoxStream;
use std::fmt::Debug;
use std::sync::Arc;

/// Extension point for building user-defined task unit streams consumed by a
/// [`crate::TaskUnitFeed`] embedded in a leaf [`datafusion::physical_plan::ExecutionPlan`].
///
/// Implement this trait on a type that knows how to produce the per-partition stream of
/// task items (e.g. file addresses, external queries, key ranges) that the leaf plan needs
/// at runtime. Then wrap the implementation with [`crate::TaskUnitFeed::new`] and store
/// the resulting [`crate::TaskUnitFeed`] as a field of your [`ExecutionPlan`] node.
///
/// In a distributed context the provider is only invoked on the **coordinating** stage
/// that initiates the query. The task units it produces are serialised and streamed over
/// the nettask to the task_schedulers, which expose the same typed stream to the leaf plan as if
/// it were running locally.
///
/// See [`TaskUnitFeedProvider::feed`] for the per-call contract.
pub trait TaskUnitFeedProvider: Send + Sync + Debug {
    type TaskUnit: TaskUnit + Default + 'static;

    /// Builds a [`TaskUnit`] stream for the given `partition`.
    ///
    /// This method is never invoked in a remote task_scheduler. On task_schedulers, the equivalent
    /// leaf plan uses a remote provider that pulls the task units off the nettask —
    /// user code doesn't need to implement that case.
    ///
    /// When implementing this method, [DistributedTaskUnitFeedContext] can be extracted from
    /// the [TaskContext], and it contains information about the amount of distributed tasks to
    /// which [TaskUnit]s should be fanned out.
    ///
    /// The implementation should be prepared to return `P*T` feeds, where `P` is the number of
    /// partitions of the [datafusion::physical_plan::ExecutionPlan] to which the
    /// [TaskUnitFeedProvider] is attached and `T` is the number of tasks to which it should fanout
    ///
    /// For more information about how [TaskUnit] feeds task, refer to the [crate::TaskUnitFeed]
    /// docs.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use std::sync::Arc;
    /// # use datafusion_distributed::{DistributedTaskUnitFeedContext, TaskUnitFeedProvider};
    /// # use datafusion::common::Result;
    /// # use datafusion::execution::TaskContext;
    /// # use futures::stream::BoxStream;
    /// # use futures::StreamExt;
    ///
    /// #[derive(Debug)]
    /// struct MyFeedProvider {
    ///     output_partitions: usize
    /// };
    ///
    /// #[derive(Clone, PartialEq, ::prost::Message)]
    /// struct MyCustomTaskUnit {
    ///     #[prost(string, tag = "1")]
    ///     custom_field: String,
    /// }
    ///
    /// impl TaskUnitFeedProvider for MyFeedProvider {
    ///     type TaskUnit = MyCustomTaskUnit;
    ///
    ///     fn feed(
    ///         &self,
    ///         partition: usize,
    ///         ctx: Arc<TaskContext>,
    ///     ) -> Result<BoxStream<'static, Result<Self::TaskUnit>>> {
    ///         let feed_ctx = DistributedTaskUnitFeedContext::from_ctx(&ctx);
    ///
    ///         // this method will be called `feed_ctx.fan_out_tasks * self.output_partitions`
    ///         // times.
    ///         Ok(futures::stream::empty().boxed())
    ///     }
    /// }
    /// ```
    fn feed(
        &self,
        partition: usize,
        ctx: Arc<TaskContext>,
    ) -> Result<BoxStream<'static, Result<Self::TaskUnit>>>;

    /// DataFusion metrics collected at runtime while streaming [TaskUnit]s through [Self::feed].
    fn metrics(&self) -> ExecutionPlanMetricsSet {
        ExecutionPlanMetricsSet::new()
    }
}

/// Provides contextual information about where a [TaskUnitFeedProvider] is being executed. When
/// using [TaskUnitFeedProvider] in distributed queries, it might be getting executed in the
/// coordinating stage, or it might be getting executed just locally because the query did not
/// need any remote execution.
pub struct DistributedTaskUnitFeedContext {
    /// The number of distributed tasks to which the [TaskUnitFeedProvider] should fan out.
    pub fan_out_tasks: usize,
}

impl DistributedTaskUnitFeedContext {
    /// Gets the [DistributedTaskUnitFeedContext] from the [TaskContext] as an extension.
    /// If no [DistributedTaskUnitFeedContext] is present, returns one valid for single-node
    /// execution.
    pub fn from_ctx(ctx: &Arc<TaskContext>) -> Arc<Self> {
        ctx.session_config()
            .get_extension::<Self>()
            .unwrap_or(Arc::new(DistributedTaskUnitFeedContext {
                fan_out_tasks: 1,
            }))
    }
}

use crate::TaskUnit;
use datafusion::common::Result;
use datafusion::execution::TaskContext;
use datafusion::physical_plan::metrics::ExecutionPlanMetricsSet;
use futures::stream::BoxStream;
use std::fmt::Debug;
use std::sync::Arc;

/// Extension point for building user-defined task unit streams consumed by a
/// [`crate::TaskUnitFeed`] embedded in a leaf [`datafusion::physical_plan::ExecutionPlan`].
///
/// Implement this trait on a type that knows how to produce the per-partition stream of
/// task items (e.g. file addresses, external queries, key ranges) that the leaf plan needs
/// at runtime. Then wrap the implementation with [`crate::TaskUnitFeed::new`] and store
/// the resulting [`crate::TaskUnitFeed`] as a field of your [`ExecutionPlan`] node.
///
/// In a distributed context the provider is only invoked on the **coordinating** stage
/// that initiates the query. The task units it produces are serialised and streamed over
/// the nettask to the task_schedulers, which expose the same typed stream to the leaf plan as if
/// it were running locally.
///
/// See [`TaskUnitFeedProvider::feed`] for the per-call contract.
pub trait TaskUnitFeedProvider: Send + Sync + Debug {
    type TaskUnit: TaskUnit + Default + 'static;

    /// Builds a [`TaskUnit`] stream for the given `partition`.
    ///
    /// This method is never invoked in a remote task_scheduler. On task_schedulers, the equivalent
    /// leaf plan uses a remote provider that pulls the task units off the nettask —
    /// user code doesn't need to implement that case.
    ///
    /// When implementing this method, [DistributedTaskUnitFeedContext] can be extracted from
    /// the [TaskContext], and it contains information about the amount of distributed tasks to
    /// which [TaskUnit]s should be fanned out.
    ///
    /// The implementation should be prepared to return `P*T` feeds, where `P` is the number of
    /// partitions of the [datafusion::physical_plan::ExecutionPlan] to which the
    /// [TaskUnitFeedProvider] is attached and `T` is the number of tasks to which it should fanout
    ///
    /// For more information about how [TaskUnit] feeds task, refer to the [crate::TaskUnitFeed]
    /// docs.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use std::sync::Arc;
    /// # use datafusion_distributed::{DistributedTaskUnitFeedContext, TaskUnitFeedProvider};
    /// # use datafusion::common::Result;
    /// # use datafusion::execution::TaskContext;
    /// # use futures::stream::BoxStream;
    /// # use futures::StreamExt;
    ///
    /// #[derive(Debug)]
    /// struct MyFeedProvider {
    ///     output_partitions: usize
    /// };
    ///
    /// #[derive(Clone, PartialEq, ::prost::Message)]
    /// struct MyCustomTaskUnit {
    ///     #[prost(string, tag = "1")]
    ///     custom_field: String,
    /// }
    ///
    /// impl TaskUnitFeedProvider for MyFeedProvider {
    ///     type TaskUnit = MyCustomTaskUnit;
    ///
    ///     fn feed(
    ///         &self,
    ///         partition: usize,
    ///         ctx: Arc<TaskContext>,
    ///     ) -> Result<BoxStream<'static, Result<Self::TaskUnit>>> {
    ///         let feed_ctx = DistributedTaskUnitFeedContext::from_ctx(&ctx);
    ///
    ///         // this method will be called `feed_ctx.fan_out_tasks * self.output_partitions`
    ///         // times.
    ///         Ok(futures::stream::empty().boxed())
    ///     }
    /// }
    /// ```
    fn feed(
        &self,
        partition: usize,
        ctx: Arc<TaskContext>,
    ) -> Result<BoxStream<'static, Result<Self::TaskUnit>>>;

    /// DataFusion metrics collected at runtime while streaming [TaskUnit]s through [Self::feed].
    fn metrics(&self) -> ExecutionPlanMetricsSet {
        ExecutionPlanMetricsSet::new()
    }
}

/// Provides contextual information about where a [TaskUnitFeedProvider] is being executed. When
/// using [TaskUnitFeedProvider] in distributed queries, it might be getting executed in the
/// coordinating stage, or it might be getting executed just locally because the query did not
/// need any remote execution.
pub struct DistributedTaskUnitFeedContext {
    /// The number of distributed tasks to which the [TaskUnitFeedProvider] should fan out.
    pub fan_out_tasks: usize,
}

impl DistributedTaskUnitFeedContext {
    /// Gets the [DistributedTaskUnitFeedContext] from the [TaskContext] as an extension.
    /// If no [DistributedTaskUnitFeedContext] is present, returns one valid for single-node
    /// execution.
    pub fn from_ctx(ctx: &Arc<TaskContext>) -> Arc<Self> {
        ctx.session_config()
            .get_extension::<Self>()
            .unwrap_or(Arc::new(DistributedTaskUnitFeedContext {
                fan_out_tasks: 1,
            }))
    }
}
