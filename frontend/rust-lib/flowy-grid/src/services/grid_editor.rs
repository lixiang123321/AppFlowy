use crate::dart_notification::{send_dart_notification, GridNotification};
use crate::manager::GridUser;
use crate::services::block_meta_editor::GridBlockMetaEditorManager;
use crate::services::field::{
    default_type_option_builder_from_type, type_option_builder_from_bytes, FieldBuilder, SelectOptionChangesetParams,
};
use crate::services::persistence::block_index::BlockIndexPersistence;
use crate::services::row::*;
use bytes::Bytes;
use flowy_error::{ErrorCode, FlowyError, FlowyResult};
use flowy_grid_data_model::entities::*;
use flowy_revision::{RevisionCloudService, RevisionCompactor, RevisionManager, RevisionObjectBuilder};
use flowy_sync::client_grid::{GridChangeset, GridMetaPad, TypeOptionDataDeserializer};
use flowy_sync::entities::revision::Revision;
use flowy_sync::errors::CollaborateResult;
use flowy_sync::util::make_delta_from_revisions;
use lib_infra::future::FutureResult;
use lib_ot::core::PlainTextAttributes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ClientGridEditor {
    grid_id: String,
    user: Arc<dyn GridUser>,
    pad: Arc<RwLock<GridMetaPad>>,
    rev_manager: Arc<RevisionManager>,
    block_meta_manager: Arc<GridBlockMetaEditorManager>,
}

impl ClientGridEditor {
    pub async fn new(
        grid_id: &str,
        user: Arc<dyn GridUser>,
        mut rev_manager: RevisionManager,
        persistence: Arc<BlockIndexPersistence>,
    ) -> FlowyResult<Arc<Self>> {
        let token = user.token()?;
        let cloud = Arc::new(GridRevisionCloudService { token });
        let grid_pad = rev_manager.load::<GridPadBuilder>(Some(cloud)).await?;
        let rev_manager = Arc::new(rev_manager);
        let pad = Arc::new(RwLock::new(grid_pad));
        let blocks = pad.read().await.get_block_metas().clone();

        let block_meta_manager = Arc::new(GridBlockMetaEditorManager::new(grid_id, &user, blocks, persistence).await?);
        Ok(Arc::new(Self {
            grid_id: grid_id.to_owned(),
            user,
            pad,
            rev_manager,
            block_meta_manager,
        }))
    }

    pub async fn create_field(&self, params: CreateFieldParams) -> FlowyResult<()> {
        let CreateFieldParams {
            field,
            type_option_data,
            start_field_id,
            grid_id,
        } = params;

        let _ = self
            .modify(|grid| {
                if grid.contain_field(&field.id) {
                    let deserializer = TypeOptionChangesetDeserializer(field.field_type.clone());
                    let changeset = FieldChangesetParams {
                        field_id: field.id,
                        grid_id,
                        name: Some(field.name),
                        desc: Some(field.desc),
                        field_type: Some(field.field_type),
                        frozen: Some(field.frozen),
                        visibility: Some(field.visibility),
                        width: Some(field.width),
                        type_option_data: Some(type_option_data),
                    };
                    Ok(grid.update_field(changeset, deserializer)?)
                } else {
                    // let type_option_json = type_option_json_str_from_bytes(type_option_data, &field.field_type);
                    let builder = type_option_builder_from_bytes(type_option_data, &field.field_type);
                    let field_meta = FieldBuilder::from_field(field, builder).build();
                    Ok(grid.create_field(field_meta, start_field_id)?)
                }
            })
            .await?;
        let _ = self.notify_did_update_fields().await?;
        Ok(())
    }

    pub async fn create_next_field_meta(&self, field_type: &FieldType) -> FlowyResult<FieldMeta> {
        let name = format!("Property {}", self.pad.read().await.fields().len() + 1);
        let field_meta = FieldBuilder::from_field_type(field_type).name(&name).build();
        Ok(field_meta)
    }

    pub async fn contain_field(&self, field_id: &str) -> bool {
        self.pad.read().await.contain_field(field_id)
    }

    pub async fn update_field(&self, params: FieldChangesetParams) -> FlowyResult<()> {
        let field_id = params.field_id.clone();
        let deserializer = match self.pad.read().await.get_field(&params.field_id) {
            None => return Err(ErrorCode::FieldDoesNotExist.into()),
            Some(field_meta) => TypeOptionChangesetDeserializer(field_meta.field_type.clone()),
        };

        let _ = self.modify(|grid| Ok(grid.update_field(params, deserializer)?)).await?;
        let _ = self.notify_did_update_fields().await?;
        let _ = self.notify_did_update_field(&field_id).await?;
        Ok(())
    }

    pub async fn delete_field(&self, field_id: &str) -> FlowyResult<()> {
        let _ = self.modify(|grid| Ok(grid.delete_field(field_id)?)).await?;
        let _ = self.notify_did_update_fields().await?;
        Ok(())
    }

    pub async fn switch_to_field_type(&self, field_id: &str, field_type: &FieldType) -> FlowyResult<()> {
        // let block_ids = self
        //     .get_block_metas()
        //     .await?
        //     .into_iter()
        //     .map(|block_meta| block_meta.block_id)
        //     .collect();
        // let cell_metas = self
        //     .block_meta_manager
        //     .get_cell_metas(block_ids, field_id, None)
        //     .await?;

        let type_option_json_builder = |field_type: &FieldType| -> String {
            return default_type_option_builder_from_type(field_type).entry().json_str();
        };

        let _ = self
            .modify(|grid| Ok(grid.switch_to_field(field_id, field_type.clone(), type_option_json_builder)?))
            .await?;
        let _ = self.notify_did_update_fields().await?;
        let _ = self.notify_did_update_field(field_id).await?;
        Ok(())
    }

    pub async fn duplicate_field(&self, field_id: &str) -> FlowyResult<()> {
        let _ = self.modify(|grid| Ok(grid.duplicate_field(field_id)?)).await?;
        let _ = self.notify_did_update_fields().await?;
        Ok(())
    }

    pub async fn get_field(&self, field_id: &str) -> FlowyResult<Option<FieldMeta>> {
        match self.pad.read().await.get_field(field_id) {
            None => Ok(None),
            Some(field_meta) => Ok(Some(field_meta.clone())),
        }
    }

    pub async fn create_block(&self, grid_block: GridBlockMeta) -> FlowyResult<()> {
        let _ = self.modify(|grid| Ok(grid.create_block_meta(grid_block)?)).await?;
        Ok(())
    }

    pub async fn update_block(&self, changeset: GridBlockMetaChangeset) -> FlowyResult<()> {
        let _ = self.modify(|grid| Ok(grid.update_block_meta(changeset)?)).await?;
        Ok(())
    }

    pub async fn create_row(&self, start_row_id: Option<String>) -> FlowyResult<RowOrder> {
        let field_metas = self.pad.read().await.get_field_metas(None)?;
        let block_id = self.block_id().await?;

        // insert empty row below the row whose id is upper_row_id
        let row_meta_ctx = CreateRowMetaBuilder::new(&field_metas).build();
        let row_meta = make_row_meta_from_context(&block_id, row_meta_ctx);
        let row_order = RowOrder::from(&row_meta);

        // insert the row
        let row_count = self
            .block_meta_manager
            .create_row(&block_id, row_meta, start_row_id)
            .await?;

        // update block row count
        let changeset = GridBlockMetaChangeset::from_row_count(&block_id, row_count);
        let _ = self.update_block(changeset).await?;
        Ok(row_order)
    }

    pub async fn insert_rows(&self, contexts: Vec<CreateRowMetaPayload>) -> FlowyResult<Vec<RowOrder>> {
        let block_id = self.block_id().await?;
        let mut rows_by_block_id: HashMap<String, Vec<RowMeta>> = HashMap::new();
        let mut row_orders = vec![];
        for ctx in contexts {
            let row_meta = make_row_meta_from_context(&block_id, ctx);
            row_orders.push(RowOrder::from(&row_meta));
            rows_by_block_id
                .entry(block_id.clone())
                .or_insert_with(Vec::new)
                .push(row_meta);
        }
        let changesets = self.block_meta_manager.insert_row(rows_by_block_id).await?;
        for changeset in changesets {
            let _ = self.update_block(changeset).await?;
        }
        Ok(row_orders)
    }

    pub async fn update_row(&self, changeset: RowMetaChangeset) -> FlowyResult<()> {
        self.block_meta_manager.update_row(changeset).await
    }

    pub async fn get_rows(&self, block_id: &str) -> FlowyResult<RepeatedRow> {
        let block_ids = vec![block_id.to_owned()];
        let mut grid_block_snapshot = self.grid_block_snapshots(Some(block_ids)).await?;

        // For the moment, we only support one block.
        // We can save the rows into multiple blocks and load them asynchronously in the future.
        debug_assert_eq!(grid_block_snapshot.len(), 1);
        if grid_block_snapshot.len() == 1 {
            let snapshot = grid_block_snapshot.pop().unwrap();
            let field_metas = self.get_field_metas(None).await?;
            let rows = make_rows_from_row_metas(&field_metas, &snapshot.row_metas);
            Ok(rows.into())
        } else {
            Ok(vec![].into())
        }
    }

    pub async fn get_row(&self, row_id: &str) -> FlowyResult<Option<Row>> {
        match self.block_meta_manager.get_row_meta(row_id).await? {
            None => Ok(None),
            Some(row_meta) => {
                let field_metas = self.get_field_metas(None).await?;
                let row_metas = vec![row_meta];
                let mut rows = make_rows_from_row_metas(&field_metas, &row_metas);
                debug_assert!(rows.len() == 1);
                Ok(rows.pop())
            }
        }
    }

    pub async fn get_cell_meta(&self, row_id: &str, field_id: &str) -> FlowyResult<Option<CellMeta>> {
        let row_meta = self.block_meta_manager.get_row_meta(row_id).await?;
        match row_meta {
            None => Ok(None),
            Some(row_meta) => {
                let cell_meta = row_meta.cell_by_field_id.get(field_id).cloned();
                Ok(cell_meta)
            }
        }
    }

    pub async fn apply_select_option(&self, params: SelectOptionChangesetParams) -> FlowyResult<()> {
        let cell_meta = self.get_cell_meta(&params.row_id, &params.field_id).await?;
        todo!()
    }

    pub async fn update_cell(&self, mut changeset: CellMetaChangeset) -> FlowyResult<()> {
        if let Some(cell_data) = changeset.data.as_ref() {
            match self.pad.read().await.get_field(&changeset.field_id) {
                None => {
                    let msg = format!("Can not find the field with id: {}", &changeset.field_id);
                    return Err(FlowyError::internal().context(msg));
                }
                Some(field_meta) => {
                    let cell_data = serialize_cell_data(cell_data, field_meta)?;
                    changeset.data = Some(cell_data);
                }
            }
        }

        let field_metas = self.get_field_metas(None).await?;
        let row_changeset: RowMetaChangeset = changeset.into();
        let _ = self
            .block_meta_manager
            .update_row_cells(&field_metas, row_changeset)
            .await?;
        Ok(())
    }

    pub async fn get_blocks(&self, block_ids: Option<Vec<String>>) -> FlowyResult<RepeatedGridBlock> {
        let block_snapshots = self.grid_block_snapshots(block_ids.clone()).await?;
        make_grid_blocks(block_ids, block_snapshots)
    }

    pub async fn get_block_metas(&self) -> FlowyResult<Vec<GridBlockMeta>> {
        let grid_blocks = self.pad.read().await.get_block_metas();
        Ok(grid_blocks)
    }

    pub async fn delete_rows(&self, row_orders: Vec<RowOrder>) -> FlowyResult<()> {
        let changesets = self.block_meta_manager.delete_rows(row_orders).await?;
        for changeset in changesets {
            let _ = self.update_block(changeset).await?;
        }
        Ok(())
    }

    pub async fn grid_data(&self) -> FlowyResult<Grid> {
        let field_orders = self.pad.read().await.get_field_orders();
        let block_orders = self
            .pad
            .read()
            .await
            .get_block_metas()
            .into_iter()
            .map(|grid_block_meta| GridBlockOrder {
                block_id: grid_block_meta.block_id,
            })
            .collect::<Vec<_>>();
        Ok(Grid {
            id: self.grid_id.clone(),
            field_orders,
            block_orders,
        })
    }

    pub async fn get_field_metas(&self, field_orders: Option<Vec<FieldOrder>>) -> FlowyResult<Vec<FieldMeta>> {
        let expected_len = match field_orders.as_ref() {
            None => 0,
            Some(field_orders) => field_orders.len(),
        };

        let field_metas = self.pad.read().await.get_field_metas(field_orders)?;
        if expected_len != 0 && field_metas.len() != expected_len {
            tracing::error!(
                "This is a bug. The len of the field_metas should equal to {}",
                expected_len
            );
            debug_assert!(field_metas.len() == expected_len);
        }
        Ok(field_metas)
    }

    pub async fn grid_block_snapshots(&self, block_ids: Option<Vec<String>>) -> FlowyResult<Vec<GridBlockSnapshot>> {
        let block_ids = match block_ids {
            None => self
                .pad
                .read()
                .await
                .get_block_metas()
                .into_iter()
                .map(|block_meta| block_meta.block_id)
                .collect::<Vec<String>>(),
            Some(block_ids) => block_ids,
        };
        let snapshots = self.block_meta_manager.make_block_snapshots(block_ids).await?;
        Ok(snapshots)
    }

    pub async fn delta_bytes(&self) -> Bytes {
        self.pad.read().await.delta_bytes()
    }

    async fn modify<F>(&self, f: F) -> FlowyResult<()>
    where
        F: for<'a> FnOnce(&'a mut GridMetaPad) -> FlowyResult<Option<GridChangeset>>,
    {
        let mut write_guard = self.pad.write().await;
        match f(&mut *write_guard)? {
            None => {}
            Some(change) => {
                let _ = self.apply_change(change).await?;
            }
        }
        Ok(())
    }

    async fn apply_change(&self, change: GridChangeset) -> FlowyResult<()> {
        let GridChangeset { delta, md5 } = change;
        let user_id = self.user.user_id()?;
        let (base_rev_id, rev_id) = self.rev_manager.next_rev_id_pair();
        let delta_data = delta.to_delta_bytes();
        let revision = Revision::new(
            &self.rev_manager.object_id,
            base_rev_id,
            rev_id,
            delta_data,
            &user_id,
            md5,
        );
        let _ = self
            .rev_manager
            .add_local_revision(&revision, Box::new(GridRevisionCompactor()))
            .await?;
        Ok(())
    }

    async fn block_id(&self) -> FlowyResult<String> {
        match self.pad.read().await.get_block_metas().last() {
            None => Err(FlowyError::internal().context("There is no grid block in this grid")),
            Some(grid_block) => Ok(grid_block.block_id.clone()),
        }
    }

    async fn notify_did_update_fields(&self) -> FlowyResult<()> {
        let field_metas = self.get_field_metas(None).await?;
        let repeated_field: RepeatedField = field_metas.into_iter().map(Field::from).collect::<Vec<_>>().into();
        send_dart_notification(&self.grid_id, GridNotification::DidUpdateFields)
            .payload(repeated_field)
            .send();
        Ok(())
    }

    async fn notify_did_update_field(&self, field_id: &str) -> FlowyResult<()> {
        let field_order = FieldOrder::from(field_id);
        let mut field_metas = self.get_field_metas(Some(field_order.into())).await?;
        debug_assert!(field_metas.len() == 1);

        if let Some(field_meta) = field_metas.pop() {
            send_dart_notification(&self.grid_id, GridNotification::DidUpdateField)
                .payload(field_meta)
                .send();
        }

        Ok(())
    }
}

#[cfg(feature = "flowy_unit_test")]
impl ClientGridEditor {
    pub fn rev_manager(&self) -> Arc<RevisionManager> {
        self.rev_manager.clone()
    }
}

pub struct GridPadBuilder();
impl RevisionObjectBuilder for GridPadBuilder {
    type Output = GridMetaPad;

    fn build_object(object_id: &str, revisions: Vec<Revision>) -> FlowyResult<Self::Output> {
        let pad = GridMetaPad::from_revisions(object_id, revisions)?;
        Ok(pad)
    }
}

struct GridRevisionCloudService {
    #[allow(dead_code)]
    token: String,
}

impl RevisionCloudService for GridRevisionCloudService {
    #[tracing::instrument(level = "trace", skip(self))]
    fn fetch_object(&self, _user_id: &str, _object_id: &str) -> FutureResult<Vec<Revision>, FlowyError> {
        FutureResult::new(async move { Ok(vec![]) })
    }
}

struct GridRevisionCompactor();
impl RevisionCompactor for GridRevisionCompactor {
    fn bytes_from_revisions(&self, revisions: Vec<Revision>) -> FlowyResult<Bytes> {
        let delta = make_delta_from_revisions::<PlainTextAttributes>(revisions)?;
        Ok(delta.to_delta_bytes())
    }
}

struct TypeOptionChangesetDeserializer(FieldType);
impl TypeOptionDataDeserializer for TypeOptionChangesetDeserializer {
    fn deserialize(&self, type_option_data: Vec<u8>) -> CollaborateResult<String> {
        // The type_option_data is serialized by protobuf. But the type_option_data should be
        // serialized by utf-8. So we must transform the data here.

        let builder = type_option_builder_from_bytes(type_option_data, &self.0);
        Ok(builder.entry().json_str())
    }
}
