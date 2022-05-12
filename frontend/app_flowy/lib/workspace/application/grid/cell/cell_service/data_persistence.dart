part of 'cell_service.dart';

abstract class _GridCellDataPersistence<D> {
  Future<Option<FlowyError>> save(D data);
}

class CellDataPersistence implements _GridCellDataPersistence<String> {
  final GridCell gridCell;

  CellDataPersistence({
    required this.gridCell,
  });
  final CellService _cellService = CellService();

  @override
  Future<Option<FlowyError>> save(String data) async {
    final fut = _cellService.updateCell(
      gridId: gridCell.gridId,
      fieldId: gridCell.field.id,
      rowId: gridCell.rowId,
      data: data,
    );

    return fut.then((result) {
      return result.fold(
        (l) => none(),
        (err) => Some(err),
      );
    });
  }
}

@freezed
class DateCellPersistenceData with _$DateCellPersistenceData {
  const factory DateCellPersistenceData({required DateTime date, String? time}) = _DateCellPersistenceData;
}

class NumberCellDataPersistence implements _GridCellDataPersistence<DateCellPersistenceData> {
  final GridCell gridCell;
  NumberCellDataPersistence({
    required this.gridCell,
  });

  @override
  Future<Option<FlowyError>> save(DateCellPersistenceData data) {
    var payload = DateChangesetPayload.create()..cellIdentifier = _cellIdentifier(gridCell);

    final date = (data.date.millisecondsSinceEpoch ~/ 1000).toString();
    payload.date = date;

    if (data.time != null) {
      payload.time = data.time!;
    }

    return GridEventUpdateDateCell(payload).send().then((result) {
      return result.fold(
        (l) => none(),
        (err) => Some(err),
      );
    });
  }
}

CellIdentifierPayload _cellIdentifier(GridCell gridCell) {
  return CellIdentifierPayload.create()
    ..gridId = gridCell.gridId
    ..fieldId = gridCell.field.id
    ..rowId = gridCell.rowId;
}